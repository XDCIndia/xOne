//! Flat account and storage tables, keyed like geth's snapshot layer: `keccak(address)` for an
//! account, `keccak(address) || keccak(slot)` for a slot (ADR-005 amendment 2026-11-12). The
//! address-taking methods hash on access. Reads here never touch a trie; the trie is only ever built
//! from these tables (`crate::trie`).

use crate::{
    changeset::{ChangeSet, HashedDiff},
    Account,
};
use alloy_primitives::{keccak256, Address, B256, U256};
use std::collections::BTreeMap;

pub trait FlatRead {
    fn hashed_account(&self, ha: B256) -> Option<Account>;
    /// Zero when absent.
    fn hashed_storage(&self, ha: B256, hs: B256) -> U256;
    /// In key order.
    fn hashed_accounts(&self) -> Box<dyn Iterator<Item = (B256, Account)> + '_>;
    /// Non-zero slots of one account, in key order.
    fn hashed_slots(&self, ha: B256) -> Box<dyn Iterator<Item = (B256, U256)> + '_>;

    fn account(&self, address: Address) -> Option<Account> {
        self.hashed_account(keccak256(address))
    }
    /// Zero when absent.
    fn storage(&self, address: Address, slot: B256) -> U256 {
        self.hashed_storage(keccak256(address), keccak256(slot))
    }
}

pub trait FlatWrite: FlatRead {
    fn set_hashed_account(&mut self, ha: B256, account: Option<Account>);
    /// A zero value removes the slot.
    fn set_hashed_slot(&mut self, ha: B256, hs: B256, value: U256);

    fn set_account(&mut self, address: Address, account: Option<Account>) {
        self.set_hashed_account(keccak256(address), account);
    }
    /// A zero value removes the slot.
    fn set_slot(&mut self, address: Address, slot: B256, value: U256) {
        self.set_hashed_slot(keccak256(address), keccak256(slot), value);
    }

    /// `diff` is already in the tables behind this flat state (a store wrote it, together with the
    /// changeset `cs` of the same block): bring this view in line. A flat state that owns its rows
    /// writes them; one that reads tables only updates its caches.
    fn commit_written(&mut self, diff: &HashedDiff, cs: &ChangeSet);

    /// The addresses and slot keys of `diff`, before it is written by hash: a flat state that
    /// lists addresses (a recording [`MemFlat`]) keeps them; any other ignores them.
    fn note_preimages(&mut self, _diff: &crate::StateDiff) {}
}

/// The addresses and slots behind the hashes of a [`MemFlat`], kept only by a recording one.
#[derive(Debug, Clone, Default)]
struct Pre {
    addresses: BTreeMap<B256, Address>,
    slots: BTreeMap<B256, B256>,
}

#[derive(Debug, Clone, Default)]
pub struct MemFlat {
    accounts: BTreeMap<B256, Account>,
    storage: BTreeMap<(B256, B256), U256>,
    /// Off by default: the node does not keep preimages in memory (its store has a table).
    pre: Option<Pre>,
}

/// Equal when the state is equal; recorded preimages are not state.
impl PartialEq for MemFlat {
    fn eq(&self, o: &Self) -> bool {
        self.accounts == o.accounts && self.storage == o.storage
    }
}
impl Eq for MemFlat {}

impl MemFlat {
    /// A flat state that remembers the address and slot of every key written through
    /// [`FlatWrite::set_account`] and [`FlatWrite::set_slot`], for the tools that must list
    /// addresses (the replay harness, dumps). Keys written by hash only stay unnamed.
    pub fn recording() -> Self {
        Self {
            pre: Some(Pre::default()),
            ..Self::default()
        }
    }

    /// The addresses and slot keys this recording flat state has seen, as the bytes that hash to
    /// their keys (20 and 32 bytes). Empty unless recording.
    pub fn preimages(&self) -> Vec<Vec<u8>> {
        self.pre
            .iter()
            .flat_map(|p| {
                p.addresses
                    .values()
                    .map(|a| a.to_vec())
                    .chain(p.slots.values().map(|s| s.to_vec()))
            })
            .collect()
    }

    /// Accounts whose address is known, in hash order. Empty unless the flat state is recording.
    pub fn accounts(&self) -> impl Iterator<Item = (Address, Account)> + '_ {
        self.accounts.iter().filter_map(|(h, x)| {
            let a = *self.pre.as_ref()?.addresses.get(h)?;
            Some((a, x.clone()))
        })
    }

    /// Slots of `address` whose slot key is known. Empty unless the flat state is recording.
    pub fn slots(&self, address: Address) -> impl Iterator<Item = (B256, U256)> + '_ {
        self.hashed_slots(keccak256(address)).filter_map(|(hs, v)| {
            let s = *self.pre.as_ref()?.slots.get(&hs)?;
            Some((s, v))
        })
    }
}

impl FlatRead for MemFlat {
    fn hashed_account(&self, ha: B256) -> Option<Account> {
        self.accounts.get(&ha).cloned()
    }
    fn hashed_storage(&self, ha: B256, hs: B256) -> U256 {
        // an unset slot reads as zero (EVM semantics)
        #[allow(clippy::disallowed_methods)]
        let v = self.storage.get(&(ha, hs)).copied().unwrap_or_default();
        v
    }
    fn hashed_accounts(&self) -> Box<dyn Iterator<Item = (B256, Account)> + '_> {
        Box::new(self.accounts.iter().map(|(a, x)| (*a, x.clone())))
    }
    fn hashed_slots(&self, ha: B256) -> Box<dyn Iterator<Item = (B256, U256)> + '_> {
        Box::new(
            self.storage
                .range((ha, B256::ZERO)..=(ha, B256::repeat_byte(0xff)))
                .map(|((_, s), v)| (*s, *v)),
        )
    }
}

impl FlatWrite for MemFlat {
    fn set_account(&mut self, address: Address, account: Option<Account>) {
        let ha = keccak256(address);
        if let (Some(p), Some(_)) = (&mut self.pre, &account) {
            p.addresses.insert(ha, address);
        }
        self.set_hashed_account(ha, account);
    }
    fn set_slot(&mut self, address: Address, slot: B256, value: U256) {
        let (ha, hs) = (keccak256(address), keccak256(slot));
        if let (Some(p), false) = (&mut self.pre, value.is_zero()) {
            p.slots.insert(hs, slot);
        }
        self.set_hashed_slot(ha, hs, value);
    }
    fn note_preimages(&mut self, diff: &crate::StateDiff) {
        let Some(p) = &mut self.pre else {
            return;
        };
        for (a, new) in &diff.accounts {
            if new.is_some() {
                p.addresses.insert(keccak256(a), *a);
            }
        }
        for (&(_, s), v) in &diff.storage {
            if !v.is_zero() {
                p.slots.insert(keccak256(s), s);
            }
        }
    }
    fn commit_written(&mut self, diff: &HashedDiff, _cs: &ChangeSet) {
        for (ha, new) in &diff.accounts {
            if new.is_none() {
                let old: Vec<_> = self.hashed_slots(*ha).map(|(hs, _)| hs).collect();
                for hs in old {
                    self.set_hashed_slot(*ha, hs, U256::ZERO);
                }
            }
        }
        for (&(ha, hs), &v) in &diff.storage {
            if !matches!(diff.accounts.get(&ha), Some(None)) {
                self.set_hashed_slot(ha, hs, v);
            }
        }
        for (ha, new) in &diff.accounts {
            self.set_hashed_account(*ha, new.clone());
        }
    }
    fn set_hashed_account(&mut self, ha: B256, account: Option<Account>) {
        match account {
            Some(a) => self.accounts.insert(ha, a),
            None => self.accounts.remove(&ha),
        };
    }
    fn set_hashed_slot(&mut self, ha: B256, hs: B256, value: U256) {
        if value.is_zero() {
            self.storage.remove(&(ha, hs));
        } else {
            self.storage.insert((ha, hs), value);
        }
    }
}
