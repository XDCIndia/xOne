//! Per-block changesets (ADR-005 `AccountChangeSets`, `StorageChangeSets`), keyed by hash.
//!
//! A changeset stores the whole previous record of every touched account and the previous value
//! of every touched slot, never a delta, so nonce, balance (wide included) and code hash revert
//! together. [`HashedDiff::apply`] returns the changeset of the block it applied;
//! [`ChangeSet::revert`] undoes exactly that. The executor's [`StateDiff`] is keyed by address
//! and slot; it is hashed once ([`StateDiff::hashed`]) and the stores only ever see hashes.

use crate::{flat::FlatWrite, Account};
use alloy_primitives::{keccak256, Address, B256, U256};
use std::collections::BTreeMap;
use xone_primitives::WideBalanceError;

/// The new values a block writes. `None` for an account deletes it together with all of its
/// storage; slots of a surviving account are listed explicitly (zero removes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateDiff {
    pub accounts: BTreeMap<Address, Option<Account>>,
    pub storage: BTreeMap<(Address, B256), U256>,
}

/// A diff keyed by `keccak(address)` and `(keccak(address), keccak(slot))`: what the flat tables
/// and the trie take. Its other use is the changeset below.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashedDiff {
    pub accounts: BTreeMap<B256, Option<Account>>,
    pub storage: BTreeMap<(B256, B256), U256>,
}

/// The values a block overwrote: the previous record of each touched account (`None` = absent)
/// and the previous value of each touched slot (zero = absent). It has the shape of a diff on
/// purpose: reverting is writing it back.
pub type ChangeSet = HashedDiff;

impl StateDiff {
    pub fn hashed(&self) -> HashedDiff {
        HashedDiff {
            accounts: self
                .accounts
                .iter()
                .map(|(a, x)| (keccak256(a), x.clone()))
                .collect(),
            storage: self
                .storage
                .iter()
                .map(|(&(a, s), v)| ((keccak256(a), keccak256(s)), *v))
                .collect(),
        }
    }

    /// Writes the diff and returns its changeset. Fails, before any write, if a balance is above
    /// the legal bound (`WideBalance::check_legal`).
    pub fn apply(&self, flat: &mut impl FlatWrite) -> Result<ChangeSet, WideBalanceError> {
        self.hashed().apply(flat)
    }
}

impl HashedDiff {
    /// See [`StateDiff::apply`].
    pub fn apply(&self, flat: &mut impl FlatWrite) -> Result<ChangeSet, WideBalanceError> {
        for a in self.accounts.values().flatten() {
            a.balance.check_legal()?;
        }
        let mut cs = ChangeSet::default();
        for (ha, new) in &self.accounts {
            cs.accounts.insert(*ha, flat.hashed_account(*ha));
            if new.is_none() {
                let old: Vec<_> = flat.hashed_slots(*ha).collect();
                for (hs, v) in old {
                    cs.storage.insert((*ha, hs), v);
                    flat.set_hashed_slot(*ha, hs, U256::ZERO);
                }
            }
        }
        for (&(ha, hs), &v) in &self.storage {
            if matches!(self.accounts.get(&ha), Some(None)) {
                continue; // storage of a deleted account is already wiped
            }
            // A wiped slot keeps the value recorded by the wipe: that is the true previous one.
            cs.storage
                .entry((ha, hs))
                .or_insert_with(|| flat.hashed_storage(ha, hs));
            flat.set_hashed_slot(ha, hs, v);
        }
        for (ha, new) in &self.accounts {
            flat.set_hashed_account(*ha, new.clone());
        }
        Ok(cs)
    }

    pub fn revert(&self, flat: &mut impl FlatWrite) {
        for (ha, old) in &self.accounts {
            flat.set_hashed_account(*ha, old.clone());
        }
        for (&(ha, hs), &v) in &self.storage {
            flat.set_hashed_slot(ha, hs, v);
        }
    }
}
