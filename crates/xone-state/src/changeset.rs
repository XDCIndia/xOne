//! Per-block changesets (ADR-005 `AccountChangeSets`, `StorageChangeSets`).
//!
//! A changeset stores the whole previous record of every touched account and the previous value
//! of every touched slot, never a delta, so nonce, balance (wide included) and code hash revert
//! together. [`StateDiff::apply`] returns the changeset of the block it applied;
//! [`ChangeSet::revert`] undoes exactly that.

use crate::{flat::FlatWrite, Account};
use alloy_primitives::{Address, B256, U256};
use std::collections::BTreeMap;
use xone_primitives::WideBalanceError;

/// The new values a block writes. `None` for an account deletes it together with all of its
/// storage; slots of a surviving account are listed explicitly (zero removes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateDiff {
    pub accounts: BTreeMap<Address, Option<Account>>,
    pub storage: BTreeMap<(Address, B256), U256>,
}

/// The values a block overwrote: the previous record of each touched account (`None` = absent)
/// and the previous value of each touched slot (zero = absent). It has the shape of a diff on
/// purpose: reverting is writing it back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub accounts: BTreeMap<Address, Option<Account>>,
    pub storage: BTreeMap<(Address, B256), U256>,
}

impl StateDiff {
    /// Writes the diff and returns its changeset. Fails, before any write, if a balance is above
    /// the legal bound (`WideBalance::check_legal`).
    pub fn apply(&self, flat: &mut impl FlatWrite) -> Result<ChangeSet, WideBalanceError> {
        for a in self.accounts.values().flatten() {
            a.balance.check_legal()?;
        }
        let mut cs = ChangeSet::default();
        for (addr, new) in &self.accounts {
            cs.accounts.insert(*addr, flat.account(*addr));
            if new.is_none() {
                let old: Vec<_> = flat.slots(*addr).collect();
                for (slot, v) in old {
                    cs.storage.insert((*addr, slot), v);
                    flat.set_slot(*addr, slot, U256::ZERO);
                }
            }
        }
        for (&(addr, slot), &v) in &self.storage {
            if matches!(self.accounts.get(&addr), Some(None)) {
                continue; // storage of a deleted account is already wiped
            }
            // A wiped slot keeps the value recorded by the wipe: that is the true previous one.
            cs.storage
                .entry((addr, slot))
                .or_insert_with(|| flat.storage(addr, slot));
            flat.set_slot(addr, slot, v);
        }
        for (addr, new) in &self.accounts {
            flat.set_account(*addr, new.clone());
        }
        Ok(cs)
    }
}

impl ChangeSet {
    pub fn revert(&self, flat: &mut impl FlatWrite) {
        for (addr, old) in &self.accounts {
            flat.set_account(*addr, old.clone());
        }
        for (&(addr, slot), &v) in &self.storage {
            flat.set_slot(addr, slot, v);
        }
    }
}
