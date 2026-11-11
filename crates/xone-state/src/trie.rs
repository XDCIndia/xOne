//! Keccak Merkle-Patricia roots over the XDC account leaf.
//!
//! The leaf bytes are encoded here ([`crate::Account::rlp`]) and handed to
//! `HashBuilder::add_leaf` as bytes, so a wide balance is never narrowed. `alloy_trie::TrieAccount`
//! and `alloy_trie::root::state_root*` must not be used after genesis (spikes/wide-balance/REPORT.md).
//!
//! ponytail: every root is computed from the whole table. [`root_of_hashed`] is the one place that
//! touches `HashBuilder`; an incremental trie (retained nodes plus a prefix set of changed hashed
//! keys) replaces its body and keeps the hashed-leaf shape of the callers.

use crate::{flat::FlatRead, Account};
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_trie::{HashBuilder, Nibbles, EMPTY_ROOT_HASH};
use std::collections::BTreeMap;

/// Root of a secure trie whose keys are already hashed. Order of `leaves` does not matter.
pub fn root_of_hashed(leaves: impl IntoIterator<Item = (B256, Vec<u8>)>) -> B256 {
    let sorted: BTreeMap<B256, Vec<u8>> = leaves.into_iter().collect();
    if sorted.is_empty() {
        return EMPTY_ROOT_HASH;
    }
    let mut hb = HashBuilder::default();
    for (k, v) in sorted {
        hb.add_leaf(Nibbles::unpack(k), &v);
    }
    hb.root()
}

/// Storage root: key `keccak(slot)`, value the RLP of the trimmed word. Zero values are absent.
pub fn storage_root(slots: impl IntoIterator<Item = (B256, U256)>) -> B256 {
    root_of_hashed(
        slots
            .into_iter()
            .filter(|(_, v)| !v.is_zero())
            .map(|(k, v)| (keccak256(k), alloy_rlp::encode(v))),
    )
}

/// State root over the accounts as given: each `storage_root` field is used unchanged.
pub fn state_root(accounts: impl IntoIterator<Item = (Address, Account)>) -> B256 {
    root_of_hashed(
        accounts
            .into_iter()
            .map(|(a, acc)| (keccak256(a), acc.rlp())),
    )
}

/// State root of a flat state. Each account's storage root is recomputed from its slots; the
/// `storage_root` field stored in the flat table is ignored.
pub fn flat_root(flat: &impl FlatRead) -> B256 {
    state_root(flat.accounts().map(|(a, mut acc)| {
        acc.storage_root = storage_root(flat.slots(a));
        (a, acc)
    }))
}
