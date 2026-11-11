//! Algorithm- and domain-separated signature interfaces (issue #17, track A).
//!
//! Nothing here changes a consensus or wire byte. `Secp256k1Keccak` is the only
//! active algorithm and its verifier is a thin wrapper over [`ecrecover`]: the
//! digest and the 65-byte `r || s || v` signature are passed through unchanged,
//! and the [`Domain`] is not mixed into the digest. Today's domain separation is
//! whatever each caller already hashes (the tx type byte, the RLP layout), as
//! listed in `docs/pq/inventory.md`.
//!
//! Not covered by this wrapper, on purpose (it is bit-for-bit `ecrecover`):
//! the low-S rule geth applies to transactions and to V2 vote and timeout
//! signatures, and the 7702 authorization checks. A caller that needs them adds
//! them; see the "Notes" column of the inventory for the geth lines.
//!
//! A recoverable signature is an ECDSA property. A PQ scheme such as ML-DSA does
//! not recover a public key, so [`Verifier::recover`] fits only schemes that do.
//! Track B (#19, #20) decides the registry-based `verify` shape; this trait does
//! not pre-empt it.

use alloy_primitives::{Address, Bytes, B256};
use thiserror::Error;

use crate::seal::{ecrecover, SealError};

/// Version of this interface, not of any algorithm.
pub const API_VERSION: u8 = 1;

/// Signature algorithm and hash binding, as a stable `u16` for later wire use.
///
/// Proposed numbering, not a protocol decision: `0` is invalid, `1` is the
/// secp256k1 ECDSA recoverable signature over a 32-byte Keccak-256 digest used by
/// XDC today, `2..=0xEFFF` is unassigned and rejected, and `0xF000..=0xFFFF`
/// ([`AlgorithmId::EXPERIMENTAL`]) is reserved for lab implementations that never
/// ship in a production build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum AlgorithmId {
    Secp256k1Keccak = 1,
}

impl AlgorithmId {
    /// Reserved for test-only lab implementations.
    pub const EXPERIMENTAL: std::ops::RangeInclusive<u16> = 0xF000..=0xFFFF;

    pub fn id(self) -> u16 {
        self as u16
    }

    /// Only an active value converts; unassigned and experimental ids are errors.
    pub fn from_id(id: u16) -> Result<Self, CryptoError> {
        match id {
            1 => Ok(Self::Secp256k1Keccak),
            id if Self::EXPERIMENTAL.contains(&id) => Err(CryptoError::Experimental(id)),
            id => Err(CryptoError::UnknownAlgorithm(id)),
        }
    }
}

/// What a signature is for. Each variant is one row group of `docs/pq/inventory.md`.
/// secp256k1 ignores the domain; a later algorithm can bind it (for example as the
/// FIPS 204 context string, at most 255 bytes) without a caller change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    /// Pre-EIP-155 legacy transaction (Frontier and Homestead signers).
    TxLegacy,
    /// EIP-155 legacy transaction. The 0x89 and 0x90 special transactions are signed
    /// as these at the pin, so [`Domain::SignTx`] has the same digest bytes today.
    TxEip155,
    /// Typed transactions: EIP-2930, EIP-1559, EIP-4844 and EIP-7702 (type byte prefix).
    TxTyped,
    /// EIP-7702 authorization tuple (`0x05` prefix).
    Eip7702Auth,
    /// V1 block creator seal (M1), signature in the last 65 bytes of `extra_data`.
    SealV1,
    /// V1 double-validation signature (M2) in `validator`. Same digest as `SealV1`.
    ValidatorV1,
    /// V2 block proposer signature in `validator`.
    SealV2,
    /// V2 vote.
    VoteV2,
    /// V2 timeout.
    TimeoutV2,
    /// V2 SyncInfo. It has no signature of its own at the pin (it carries a QC and
    /// a TC whose signatures use `VoteV2` and `TimeoutV2`); reserved so that a
    /// future SyncInfo signature cannot reuse another domain.
    SyncInfoV2,
    /// Masternode sign and randomize transactions to 0x89 and 0x90 (see `TxEip155`).
    SignTx,
    /// RLPx handshake signature (ephemeral key over the static shared secret and nonce).
    Rlpx,
    /// discv4 packet signature.
    Discv4,
    /// ENR "v4" identity scheme record signature.
    EnrV4,
}

impl Domain {
    /// Stable ASCII tag for algorithms that bind a context. Not used by secp256k1.
    pub fn tag(self) -> &'static str {
        match self {
            Self::TxLegacy => "xone/tx-legacy",
            Self::TxEip155 => "xone/tx-eip155",
            Self::TxTyped => "xone/tx-typed",
            Self::Eip7702Auth => "xone/eip7702-auth",
            Self::SealV1 => "xone/seal-v1",
            Self::ValidatorV1 => "xone/validator-v1",
            Self::SealV2 => "xone/seal-v2",
            Self::VoteV2 => "xone/vote-v2",
            Self::TimeoutV2 => "xone/timeout-v2",
            Self::SyncInfoV2 => "xone/syncinfo-v2",
            Self::SignTx => "xone/sign-tx",
            Self::Rlpx => "xone/rlpx",
            Self::Discv4 => "xone/discv4",
            Self::EnrV4 => "xone/enr-v4",
        }
    }
}

/// A signature tagged with its algorithm, so a verifier never guesses by length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub algorithm: AlgorithmId,
    pub bytes: Bytes,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("signature algorithm {got:?} is not the verifier's {expected:?}")]
    WrongAlgorithm {
        expected: AlgorithmId,
        got: AlgorithmId,
    },
    #[error("algorithm id {0} is unassigned")]
    UnknownAlgorithm(u16),
    #[error("algorithm id {0} is in the experimental range and not valid here")]
    Experimental(u16),
    #[error(transparent)]
    Recover(#[from] SealError),
    #[error("signing failed")]
    Sign,
}

pub trait Signer {
    fn algorithm(&self) -> AlgorithmId;
    fn sign(&self, domain: Domain, digest: &B256) -> Result<Signature, CryptoError>;
}

pub trait Verifier {
    fn algorithm(&self) -> AlgorithmId;
    fn recover(
        &self,
        domain: Domain,
        digest: &B256,
        sig: &Signature,
    ) -> Result<Address, CryptoError>;
}

/// The only active verifier: the existing [`ecrecover`], unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct Secp256k1Verifier;

impl Verifier for Secp256k1Verifier {
    fn algorithm(&self) -> AlgorithmId {
        AlgorithmId::Secp256k1Keccak
    }

    fn recover(
        &self,
        _domain: Domain,
        digest: &B256,
        sig: &Signature,
    ) -> Result<Address, CryptoError> {
        if sig.algorithm != self.algorithm() {
            return Err(CryptoError::WrongAlgorithm {
                expected: self.algorithm(),
                got: sig.algorithm,
            });
        }
        Ok(ecrecover(digest, &sig.bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use k256::ecdsa::SigningKey;

    /// Test-only signer: no key handling ships in this crate (custody is #40).
    struct TestSigner(SigningKey);

    impl Signer for TestSigner {
        fn algorithm(&self) -> AlgorithmId {
            AlgorithmId::Secp256k1Keccak
        }
        fn sign(&self, _domain: Domain, digest: &B256) -> Result<Signature, CryptoError> {
            let (sig, id) = self
                .0
                .sign_prehash_recoverable(digest.as_slice())
                .map_err(|_| CryptoError::Sign)?;
            let mut bytes = sig.to_bytes().to_vec();
            bytes.push(id.to_byte());
            Ok(Signature {
                algorithm: self.algorithm(),
                bytes: bytes.into(),
            })
        }
    }

    const ALL: [Domain; 14] = [
        Domain::TxLegacy,
        Domain::TxEip155,
        Domain::TxTyped,
        Domain::Eip7702Auth,
        Domain::SealV1,
        Domain::ValidatorV1,
        Domain::SealV2,
        Domain::VoteV2,
        Domain::TimeoutV2,
        Domain::SyncInfoV2,
        Domain::SignTx,
        Domain::Rlpx,
        Domain::Discv4,
        Domain::EnrV4,
    ];

    #[test]
    fn sign_then_recover_is_domain_independent_and_matches_ecrecover() {
        let key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let want = Address::from_slice(
            &keccak256(&key.verifying_key().to_encoded_point(false).as_bytes()[1..])[12..],
        );
        let digest = keccak256(b"digest");
        let signer = TestSigner(key);
        let first = signer.sign(Domain::SealV1, &digest).unwrap();
        for d in ALL {
            // ECDSA with RFC 6979 is deterministic: the domain changes no byte.
            assert_eq!(signer.sign(d, &digest).unwrap(), first, "{d:?}");
            let got = Secp256k1Verifier.recover(d, &digest, &first).unwrap();
            assert_eq!(got, want, "{d:?}");
            assert_eq!(got, ecrecover(&digest, &first.bytes).unwrap());
        }
    }

    #[test]
    fn tags_are_unique_and_ascii() {
        let mut tags: Vec<_> = ALL.iter().map(|d| d.tag()).collect();
        assert!(tags.iter().all(|t| t.is_ascii() && t.len() <= 255));
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), ALL.len());
    }

    #[test]
    fn algorithm_ids_reject_unassigned_and_experimental() {
        assert_eq!(AlgorithmId::from_id(1), Ok(AlgorithmId::Secp256k1Keccak));
        assert_eq!(AlgorithmId::Secp256k1Keccak.id(), 1);
        assert_eq!(
            AlgorithmId::from_id(0),
            Err(CryptoError::UnknownAlgorithm(0))
        );
        assert_eq!(
            AlgorithmId::from_id(2),
            Err(CryptoError::UnknownAlgorithm(2))
        );
        for id in [0xF000, 0xFFFF] {
            assert_eq!(AlgorithmId::from_id(id), Err(CryptoError::Experimental(id)));
        }
    }

    #[test]
    fn malformed_signatures_fail_like_ecrecover() {
        let digest = B256::ZERO;
        for bytes in [vec![], vec![0u8; 64], vec![0u8; 65], vec![1u8; 66]] {
            let sig = Signature {
                algorithm: AlgorithmId::Secp256k1Keccak,
                bytes: bytes.clone().into(),
            };
            assert_eq!(
                Secp256k1Verifier.recover(Domain::VoteV2, &digest, &sig),
                Err(CryptoError::Recover(
                    ecrecover(&digest, &bytes).unwrap_err()
                ))
            );
        }
    }
}
