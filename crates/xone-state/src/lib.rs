//! State layer of xOne (XB-07): the XDC account leaf, the keccak Merkle-Patricia commitment, flat
//! account and storage tables, per-block changesets for unwind, and a provider that opens the
//! post-state of any retained block (or of a side-branch block, on its own parent) without replay.
//!
//! The XDC system-contract readers (0x88, 0x89, 0x90) are in [`system_contracts`] (XB-08).
//!
//! Everything here is memory only. The MDBX binding implements [`flat::FlatWrite`] later.
#![forbid(unsafe_code)]

pub mod account;
pub mod changeset;
pub mod flat;
pub mod mpt;
pub mod provider;
pub mod system_contracts;
pub mod trie;

pub use account::{Account, SlimError};
pub use changeset::{ChangeSet, HashedDiff, StateDiff};
pub use flat::{FlatRead, FlatWrite, MemFlat};
pub use provider::{CommitError, Missing, StateProvider, StateRead, StateView};
