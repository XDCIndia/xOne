//! Seal hashes and signer recovery (geth-XDC `consensus/XDPoS/xdpos.go`
//! `encodeSigHeader`, `ecrecover`, and `engines/engine_v2/engine.go` `sigHash`,
//! `ecrecover`).

use alloy_primitives::{keccak256, Address, B256};
use alloy_rlp::Encodable;
use secp256k1::{
    ecdsa::{RecoverableSignature, RecoveryId},
    ffi::types::AlignedType,
    Message, Secp256k1, Verification,
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
/// The public key is recovered by libsecp256k1 (`secp256k1_ecdsa_recover`), the library geth calls
/// (`crypto/signature_cgo.go`). `r` and `s` must be in `1..N`; a high `s` recovers like its low twin.
/// It is about twice as fast as the k256 recovery it replaces; the tests compare the two on every case.
pub fn ecrecover(hash: &B256, sig: &[u8]) -> Result<Address, SealError> {
    if sig.len() != EXTRA_SEAL || sig[64] > 3 {
        return Err(SealError::Invalid);
    }
    // A verify context in a stack buffer: no allocation, no randomisation, no global. A library whose
    // context does not fit gets an allocated one (`ctx_fits` keeps the buffer large enough).
    let mut buf = [AlignedType::zeroed(); CTX_WORDS];
    let out = match Secp256k1::preallocated_verification_only(&mut buf) {
        Ok(ctx) => recover(&ctx, hash, sig),
        Err(_) => recover(&Secp256k1::verification_only(), hash, sig),
    };
    out
}

/// Words of a libsecp256k1 verify context (13 in secp256k1 0.30).
const CTX_WORDS: usize = 32;

fn recover<C: Verification>(
    ctx: &Secp256k1<C>,
    hash: &B256,
    sig: &[u8],
) -> Result<Address, SealError> {
    let id = RecoveryId::try_from(i32::from(sig[64])).map_err(|_| SealError::Invalid)?;
    let sig = RecoverableSignature::from_compact(&sig[..64], id).map_err(|_| SealError::Invalid)?;
    let key = ctx
        .recover_ecdsa(&Message::from_digest(hash.0), &sig)
        .map_err(|_| SealError::Invalid)?;
    Ok(Address::from_slice(
        &keccak256(&key.serialize_uncompressed()[1..])[12..],
    ))
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
    #[cfg_attr(miri, ignore = "libsecp256k1 is C, which miri cannot run")]
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

    /// k256's own recovery (with its verify), the implementation before libsecp256k1, as the reference.
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
    #[cfg_attr(miri, ignore = "libsecp256k1 is C, which miri cannot run")]
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

    /// `r` below `P - N`, so `v` 2 and 3 take the `r + N` branch: the same answer as k256 for every
    /// `v`, whether a point exists at that x or not.
    #[test]
    #[cfg_attr(miri, ignore = "libsecp256k1 is C, which miri cannot run")]
    fn small_r_equals_k256() {
        let hash = keccak256(b"small r");
        for r in 1u8..=64 {
            for v in 0u8..=3 {
                let mut x = vec![0u8; 65];
                x[31] = r;
                x[32..64].copy_from_slice(keccak256([r]).as_slice());
                x[64] = v;
                assert_eq!(ecrecover(&hash, &x).ok(), reference(&hash, &x), "{r} {v}");
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "libsecp256k1 is C, which miri cannot run")]
    fn ctx_fits() {
        assert!(Secp256k1::preallocate_verification_size() <= CTX_WORDS);
    }

    fn hex_n() -> [u8; 32] {
        alloy_primitives::hex!("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
    }
}
