//! XDC consensus data types with byte-exact RLP and hashing: the block header,
//! the seal hashes with signer recovery, and the XDPoS V2 message types.
//! Other crates call these; none re-encodes a header by hand.

#![forbid(unsafe_code)]

mod wide_balance;
pub use wide_balance::{WideBalance, WideBalanceError, MAX_WIDE_BITS};

mod crypto;
mod header;
/// ADR-007 resource primitives, moved here from xone-node so that xone-net (rank 6) does not
/// depend on the node crate (rank 8).
#[allow(
    clippy::as_conversions,
    clippy::expect_used,
    reason = "moved unchanged from xone-node, which does not opt into the workspace lints; the typed-cast rewrite is tracked under #9"
)]
pub mod limits;
mod seal;
mod v2;

pub use crypto::{
    AlgorithmId, CryptoError, Domain, Secp256k1Verifier, Signature, Signer, Verifier, API_VERSION,
};
pub use header::{BlockNonce, Header};
pub use seal::{ecrecover, SealError, EXTRA_SEAL};
pub use v2::{
    BlockInfo, ExtraFieldsV2, ExtraV2Error, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote,
};
