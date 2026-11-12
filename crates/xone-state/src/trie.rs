//! Keccak Merkle-Patricia roots over the XDC account leaf.
//!
//! The leaf bytes are encoded here ([`crate::Account::rlp`]) and handed to
//! `HashBuilder::add_leaf` as bytes, so a wide balance is never narrowed. `alloy_trie::TrieAccount`
//! and `alloy_trie::root::state_root*` must not be used after genesis (spikes/wide-balance/REPORT.md).
//!
//! These functions compute a root from the whole table: they are the reference. The head's root
//! comes from the incremental trie ([`crate::mpt`]), which must equal them after every diff.

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
    storage_root_hashed(slots.into_iter().map(|(k, v)| (keccak256(k), v)))
}

/// [`storage_root`] over slots whose keys are already `keccak(slot)`.
pub fn storage_root_hashed(slots: impl IntoIterator<Item = (B256, U256)>) -> B256 {
    root_of_hashed(
        slots
            .into_iter()
            .filter(|(_, v)| !v.is_zero())
            .map(|(k, v)| (k, alloy_rlp::encode(v))),
    )
}

/// State root over the accounts as given: each `storage_root` field is used unchanged.
pub fn state_root(accounts: impl IntoIterator<Item = (Address, Account)>) -> B256 {
    state_root_hashed(accounts.into_iter().map(|(a, acc)| (keccak256(a), acc)))
}

/// [`state_root`] over accounts whose keys are already `keccak(address)`.
pub fn state_root_hashed(accounts: impl IntoIterator<Item = (B256, Account)>) -> B256 {
    root_of_hashed(accounts.into_iter().map(|(h, acc)| (h, acc.rlp())))
}

/// State root of a flat state. Each account's storage root is recomputed from its slots; the
/// `storage_root` field stored in the flat table is ignored.
pub fn flat_root(flat: &impl FlatRead) -> B256 {
    state_root_hashed(flat.hashed_accounts().map(|(h, mut acc)| {
        acc.storage_root = storage_root_hashed(flat.hashed_slots(h));
        (h, acc)
    }))
}

/// State root of `flat` with `diff` applied, without writing: what [`crate::StateDiff::apply`] followed
/// by [`flat_root`] gives. A deleted account is dropped with its storage, a listed account takes the
/// new record, and slots in `diff.storage` overlay the flat ones (zero removes).
pub fn root_after(flat: &impl FlatRead, diff: &crate::StateDiff) -> B256 {
    let diff = diff.hashed();
    let storage_of = |a: B256| {
        let mut slots: BTreeMap<B256, U256> = flat.hashed_slots(a).collect();
        for (&(_, s), &v) in diff
            .storage
            .range((a, B256::ZERO)..=(a, B256::repeat_byte(0xff)))
        {
            slots.insert(s, v);
        }
        storage_root_hashed(slots)
    };
    let mut leaves: BTreeMap<B256, Account> = BTreeMap::new();
    for (a, acc) in flat.hashed_accounts() {
        match diff.accounts.get(&a) {
            Some(None) => {}
            Some(Some(new)) => {
                leaves.insert(a, new.clone());
            }
            None => {
                leaves.insert(a, acc);
            }
        }
    }
    for (a, new) in &diff.accounts {
        if let Some(new) = new {
            leaves.insert(*a, new.clone());
        }
    }
    state_root_hashed(leaves.into_iter().map(|(a, mut acc)| {
        acc.storage_root = storage_of(a);
        (a, acc)
    }))
}
