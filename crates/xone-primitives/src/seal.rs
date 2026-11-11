//! Seal hashes and signer recovery (geth-XDC `consensus/XDPoS/xdpos.go`
//! `encodeSigHeader`, `ecrecover`, and `engines/engine_v2/engine.go` `sigHash`,
//! `ecrecover`).

use alloy_primitives::{keccak256, Address, B256};
use alloy_rlp::Encodable;
use k256::{
    elliptic_curve::{
        bigint::CheckedAdd,
        group::Group,
        ops::{LinearCombination, Reduce},
        point::DecompressPoint,
        sec1::ToEncodedPoint,
        subtle::Choice,
        Curve, FieldBytesEncoding, PrimeField,
    },
    AffinePoint, FieldBytes, ProjectivePoint, Scalar, Secp256k1, U256 as K256Uint,
};
use thiserror::Error;

use crate::header::{enc, list};
use crate::Header;

/// Length of the V1 proposer signature at the end of `extra_data`.
pub const EXTRA_SEAL: usize = 65;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SealError {
    #[error("no signature in the header")]
    Missing,
    #[error("signature does not recover a public key")]
    Invalid,
}

/// geth `crypto.Ecrecover` followed by the address derivation: `sig` is
/// `r || s || v` with `v` in 0..=3, `hash` the signed digest.
///
/// The public key is recovered as libsecp256k1 does it (`secp256k1_ecdsa_recover`, which geth
/// calls): `Q = r^-1 (s R - z G)` with `R` the point of x-coordinate `r` (plus the order when `v & 2`)
/// and the parity `v & 1`. `r` and `s` must be in `1..N`; a high `s` recovers like its low twin.
/// k256's `recover_from_prehash` computes the same point and then verifies the signature against it
/// again, which doubles the cost of every recovery and cannot fail for a point so recovered.
pub fn ecrecover(hash: &B256, sig: &[u8]) -> Result<Address, SealError> {
    let key = recover_point(hash, sig).ok_or(SealError::Invalid)?;
    let point = key.to_encoded_point(false);
    Ok(Address::from_slice(
        &keccak256(&point.as_bytes()[1..])[12..],
    ))
}

fn recover_point(hash: &B256, sig: &[u8]) -> Option<AffinePoint> {
    if sig.len() != EXTRA_SEAL || sig[64] > 3 {
        return None;
    }
    let bytes = |b: &[u8]| {
        let mut f = FieldBytes::default();
        f.copy_from_slice(b);
        f
    };
    let scalar = |b: &[u8]| {
        Option::<Scalar>::from(Scalar::from_repr(bytes(b))).filter(|x| !bool::from(x.is_zero()))
    };
    let (r, s) = (scalar(&sig[..32])?, scalar(&sig[32..64])?);
    let mut x = r.to_repr();
    if sig[64] & 2 != 0 {
        let x0 = <K256Uint as FieldBytesEncoding<Secp256k1>>::decode_field_bytes(&x);
        let wide = Option::<K256Uint>::from(x0.checked_add(&Secp256k1::ORDER))?;
        x = <K256Uint as FieldBytesEncoding<Secp256k1>>::encode_field_bytes(&wide);
    }
    let big_r =
        Option::<AffinePoint>::from(AffinePoint::decompress(&x, Choice::from(sig[64] & 1)))?;
    let z = <Scalar as Reduce<K256Uint>>::reduce_bytes(&bytes(hash.as_slice()));
    let r_inv = Option::<Scalar>::from(r.invert())?;
    let q = ProjectivePoint::lincomb(
        &ProjectivePoint::GENERATOR,
        &-(r_inv * z),
        &ProjectivePoint::from(big_r),
        &(r_inv * s),
    );
    (!bool::from(q.is_identity())).then(|| q.to_affine())
}

impl Header {
    /// V1 seal hash (`SealHash`): the 15 standard fields with the last 65 bytes
    /// of `extra_data` removed, then each of base fee, withdrawals hash, blob
    /// gas used, excess blob gas and parent beacon root that is set, without
    /// placeholders. Later optional fields, `validators`, `validator` and
    /// `penalties` are not sealed.
    pub fn seal_hash(&self) -> B256 {
        let extra = match self.extra_data.len().checked_sub(EXTRA_SEAL) {
            Some(n) => &self.extra_data[..n],
            None => &self.extra_data[..],
        };
        let mut p = self.standard_payload(extra);
        let tail = [
            self.base_fee.as_ref().map(enc),
            self.withdrawals_hash.as_ref().map(enc),
            self.blob_gas_used.as_ref().map(enc),
            self.excess_blob_gas.as_ref().map(enc),
            self.parent_beacon_root.as_ref().map(enc),
        ];
        for item in tail.iter().flatten() {
            p.extend_from_slice(item);
        }
        keccak256(list(&p))
    }

    /// V2 signature hash (`engine_v2.sigHash`): the 15 standard fields with the
    /// whole `extra_data`, then `validators`, `penalties` and the base fee if
    /// set. `validator` holds the signature and is not part of it.
    pub fn v2_sig_hash(&self) -> B256 {
        let mut p = self.standard_payload(&self.extra_data);
        self.validators.encode(&mut p);
        self.penalties.encode(&mut p);
        if let Some(fee) = &self.base_fee {
            fee.encode(&mut p);
        }
        keccak256(list(&p))
    }

    /// Creator of a V1 block: signer of the seal hash, signature in `extra_data`.
    pub fn v1_signer(&self) -> Result<Address, SealError> {
        let n = self
            .extra_data
            .len()
            .checked_sub(EXTRA_SEAL)
            .ok_or(SealError::Missing)?;
        ecrecover(&self.seal_hash(), &self.extra_data[n..])
    }

    /// Creator of a V2 block: signer of the V2 signature hash, signature in `validator`.
    pub fn v2_signer(&self) -> Result<Address, SealError> {
        if self.validator.is_empty() {
            return Err(SealError::Missing);
        }
        ecrecover(&self.v2_sig_hash(), &self.validator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    /// geth recovers the same address from `(r, s, v)` and from `(r, N - s, v ^ 1)`; only a
    /// caller that checks low-S (a QC) refuses the second.
    #[test]
    fn high_s_signature_recovers_the_same_address() {
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let hash = keccak256(b"high s");
        let (sig, id) = key.sign_prehash_recoverable(hash.as_slice()).unwrap();
        let mut low = sig.to_bytes().to_vec();
        low.push(id.to_byte());
        let want = ecrecover(&hash, &low).unwrap();

        let n = alloy_primitives::U256::from_be_slice(&hex_n());
        let s = alloy_primitives::U256::from_be_slice(&low[32..64]);
        let mut high = low.clone();
        high[32..64].copy_from_slice(&(n - s).to_be_bytes::<32>());
        high[64] ^= 1;
        assert_eq!(ecrecover(&hash, &high), Ok(want));
    }

    /// The previous implementation, k256's own recovery (with its verify), as the reference.
    fn reference(hash: &B256, sig: &[u8]) -> Option<Address> {
        use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
        let signature = Signature::from_slice(sig.get(..64)?).ok()?;
        let id = RecoveryId::from_byte(*sig.get(64)?)?;
        let (signature, id) = match signature.normalize_s() {
            Some(low) => (low, RecoveryId::from_byte(sig[64] ^ 1)?),
            None => (signature, id),
        };
        let key = VerifyingKey::recover_from_prehash(hash.as_slice(), &signature, id).ok()?;
        let point = key.to_encoded_point(false);
        Some(Address::from_slice(
            &keccak256(&point.as_bytes()[1..])[12..],
        ))
    }

    /// 2,000 keys and digests, each signature in its low and high form and with every `v` byte
    /// 0..=4, plus a zero and an out-of-range `r` and `s`: the same answer as k256's recovery.
    #[test]
    fn recovery_equals_k256() {
        let n = alloy_primitives::U256::from_be_slice(&hex_n());
        for i in 0u32..2_000 {
            let seed = keccak256(i.to_be_bytes());
            let Ok(key) = SigningKey::from_slice(seed.as_slice()) else {
                continue;
            };
            let hash = keccak256(seed);
            let (sig, id) = key.sign_prehash_recoverable(hash.as_slice()).unwrap();
            let mut low = sig.to_bytes().to_vec();
            low.push(id.to_byte());
            let mut high = low.clone();
            let s = alloy_primitives::U256::from_be_slice(&low[32..64]);
            high[32..64].copy_from_slice(&(n - s).to_be_bytes::<32>());
            for base in [&low, &high] {
                for v in 0u8..=4 {
                    let mut x = base.clone();
                    x[64] = v;
                    assert_eq!(ecrecover(&hash, &x).ok(), reference(&hash, &x), "{i} {v}");
                }
            }
            for (at, bytes) in [(0, [0u8; 32]), (32, [0u8; 32]), (0, hex_n()), (32, hex_n())] {
                let mut x = low.clone();
                x[at..at + 32].copy_from_slice(&bytes);
                assert_eq!(ecrecover(&hash, &x).ok(), reference(&hash, &x), "{i} {at}");
                assert!(ecrecover(&hash, &x).is_err());
            }
        }
    }

    fn hex_n() -> [u8; 32] {
        alloy_primitives::hex!("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
    }
}
