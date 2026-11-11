//! Seal hashes and signer recovery (geth-XDC `consensus/XDPoS/xdpos.go`
//! `encodeSigHeader`, `ecrecover`, and `engines/engine_v2/engine.go` `sigHash`,
//! `ecrecover`).

use alloy_primitives::{keccak256, Address, B256};
use alloy_rlp::Encodable;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
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
pub fn ecrecover(hash: &B256, sig: &[u8]) -> Result<Address, SealError> {
    if sig.len() != EXTRA_SEAL {
        return Err(SealError::Invalid);
    }
    let recover = || {
        let signature = Signature::from_slice(&sig[..64]).ok()?;
        let id = RecoveryId::from_byte(sig[64])?;
        VerifyingKey::recover_from_prehash(hash.as_slice(), &signature, id).ok()
    };
    let key = recover().ok_or(SealError::Invalid)?;
    let point = key.to_encoded_point(false);
    Ok(Address::from_slice(
        &keccak256(&point.as_bytes()[1..])[12..],
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
