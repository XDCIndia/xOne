//! Side blocks stored without state, and the caps that bound them. Spec: `spec/chain/insert.md`.
//!
//! A block whose parent state is not available (pruned, or the parent itself is stateless) is
//! stored without state. geth re-executes such a branch from the nearest ancestor whose state it
//! can recover (`insertSideChain`, `core/blockchain.go:3202-3347`); this store cannot recover
//! pruned state, so these blocks are kept for lookup only and are never adopted.

use alloy_primitives::B256;

use crate::store::Store;

/// geth `sidechainChildCap` (`core/blockchain.go:153`, check at `:3236-3256`, CertiK XDC-19): at most this many children
/// without state per parent.
pub const SIDECHAIN_CHILD_CAP: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SideError {
    #[error("sidechain child cap {SIDECHAIN_CHILD_CAP} exceeded for parent {0}")]
    ChildCap(B256),
    /// geth "sidechain ghost-state attack" (`:3211-3231`).
    #[error("sidechain block {0} has the state root of the canonical block at its height")]
    GhostState(u64),
}

/// Checks the two geth rules for storing a stateless block at `number` with state root `root`.
/// `Ok(true)` means the block is the canonical block itself (a re-import with pruned state).
pub fn admit(
    store: &impl Store,
    hash: B256,
    number: u64,
    root: B256,
    parent: B256,
    head_number: u64,
) -> Result<bool, SideError> {
    if number <= head_number {
        if let Some(c) = store.canonical_hash(number) {
            if c == hash {
                return Ok(true);
            }
            if store.header(c).is_some_and(|h| h.state_root == root) {
                return Err(SideError::GhostState(number));
            }
        }
    }
    if store.header(hash).is_none() && store.stateless_children(parent) >= SIDECHAIN_CHILD_CAP {
        return Err(SideError::ChildCap(parent));
    }
    Ok(false)
}
