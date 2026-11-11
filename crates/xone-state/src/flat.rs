//! Flat account and storage tables (ADR-005 `PlainAccountState`, `PlainStorageState`). Reads here
//! never touch a trie; the trie is only ever built from these tables (`crate::trie`).

use crate::Account;
use alloy_primitives::{Address, B256, U256};
use std::collections::BTreeMap;

pub trait FlatRead {
    fn account(&self, address: Address) -> Option<Account>;
    /// Zero when absent.
    fn storage(&self, address: Address, slot: B256) -> U256;
    fn accounts(&self) -> Box<dyn Iterator<Item = (Address, Account)> + '_>;
    /// Non-zero slots of one account.
    fn slots(&self, address: Address) -> Box<dyn Iterator<Item = (B256, U256)> + '_>;
}

pub trait FlatWrite: FlatRead {
    fn set_account(&mut self, address: Address, account: Option<Account>);
    /// A zero value removes the slot.
    fn set_slot(&mut self, address: Address, slot: B256, value: U256);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemFlat {
    accounts: BTreeMap<Address, Account>,
    storage: BTreeMap<(Address, B256), U256>,
}

impl FlatRead for MemFlat {
    fn account(&self, address: Address) -> Option<Account> {
        self.accounts.get(&address).cloned()
    }
    fn storage(&self, address: Address, slot: B256) -> U256 {
        // an unset slot reads as zero (EVM semantics)
        #[allow(clippy::disallowed_methods)]
        let v = self
            .storage
            .get(&(address, slot))
            .copied()
            .unwrap_or_default();
        v
    }
    fn accounts(&self) -> Box<dyn Iterator<Item = (Address, Account)> + '_> {
        Box::new(self.accounts.iter().map(|(a, x)| (*a, x.clone())))
    }
    fn slots(&self, address: Address) -> Box<dyn Iterator<Item = (B256, U256)> + '_> {
        Box::new(
            self.storage
                .range((address, B256::ZERO)..=(address, B256::repeat_byte(0xff)))
                .map(|((_, s), v)| (*s, *v)),
        )
    }
}

impl FlatWrite for MemFlat {
    fn set_account(&mut self, address: Address, account: Option<Account>) {
        match account {
            Some(a) => self.accounts.insert(address, a),
            None => self.accounts.remove(&address),
        };
    }
    fn set_slot(&mut self, address: Address, slot: B256, value: U256) {
        if value.is_zero() {
            self.storage.remove(&(address, slot));
        } else {
            self.storage.insert((address, slot), value);
        }
    }
}
