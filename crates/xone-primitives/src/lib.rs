//! XDC consensus data types with byte-exact RLP and hashing: the block header,
//! the seal hashes with signer recovery, and the XDPoS V2 message types.
//! Other crates call these; none re-encodes a header by hand.

#![forbid(unsafe_code)]

mod wide_balance;
pub use wide_balance::{WideBalance, WideBalanceError, MAX_WIDE_BITS};

mod header;
mod seal;
mod v2;

pub use header::{BlockNonce, Header};
pub use seal::{ecrecover, SealError, EXTRA_SEAL};
pub use v2::{
    BlockInfo, ExtraFieldsV2, ExtraV2Error, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote,
};
