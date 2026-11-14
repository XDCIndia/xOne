//! Storage interface of xOne (ADR-005). Upper crates see only these traits.
//!
//! This crate holds the traits, the manifest byte format, the pure restart reconciliation decision
//! and its two implementations: the MDBX environment ([`mdbx`]) and the static-file segments
//! ([`segments`]). `libmdbx` is the one dependency that holds `unsafe`; this crate does not.
#![forbid(unsafe_code)]

#[cfg(feature = "fault-injection")]
pub mod fault;
mod manifest;
pub mod mdbx;
mod recover;
pub mod safety;
pub mod segments;

pub use alloy_primitives::{Address, B256};
pub use manifest::{
    DecodeError, Manifest, Segment, SegmentEntry, FORMAT_VERSION, GEN_KEY, MANIFEST_KEY,
    MANIFEST_LEN,
};
pub use recover::{
    reconcile, Action, ManifestState, Recovery, Refuse, SegmentTail, QUARANTINE_OVER_FRAMES,
};
pub use xone_primitives::Header;

/// Why the store cannot be trusted for signing or serving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// Start-up reconciliation has not finished.
    Reconciling,
    /// ENOSPC on a write or sync.
    NoSpace,
    /// Any other I/O error (EIO and similar).
    Io,
    /// A sync call failed. The store stays failed until restart and verification (ADR-005 section 9).
    FsyncFailed,
    /// Group commit: the newest commits are not on the device yet (a follower that does not sign).
    GroupCommit,
}

/// Status read by the signer interface (XA-07): no signing while degraded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Durable,
    Degraded(Cause),
}

impl Durability {
    pub fn may_sign(self) -> bool {
        self == Durability::Durable
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DbError {
    #[error("store degraded: {0:?}")]
    Degraded(Cause),
    #[error("refused to start: {0:?}")]
    Refused(Refuse),
    #[error("block {got} does not extend head {head}")]
    NotExtending { head: u64, got: u64 },
    #[error("cannot unwind to {0}: above head or below retained history")]
    BadUnwindTarget(u64),
}

pub type Result<T> = core::result::Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_durable_may_sign() {
        assert!(Durability::Durable.may_sign());
        for c in [
            Cause::Reconciling,
            Cause::NoSpace,
            Cause::Io,
            Cause::FsyncFailed,
        ] {
            assert!(!Durability::Degraded(c).may_sign());
        }
    }
}
