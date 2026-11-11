//! Shared by the test files: a state over a map.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    reason = "test code"
)]

use alloy_primitives::{keccak256, Address, B256, U256};
use std::{collections::HashMap, path::PathBuf};
use xone_state::{Account, StateRead};

#[derive(Default, Clone)]
pub struct MapState(pub HashMap<Address, Account>);

impl StateRead for MapState {
    fn account(&self, a: Address) -> Option<Account> {
        self.0.get(&a).cloned()
    }
    fn storage(&self, _: Address, _: B256) -> U256 {
        U256::ZERO
    }
}

pub fn vectors(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/txpool")
        .join(name)
}

pub fn account(nonce: u64, balance: U256, code: &[u8]) -> Account {
    Account {
        nonce,
        balance: balance.into(),
        code_hash: if code.is_empty() {
            Account::default().code_hash
        } else {
            keccak256(code)
        },
        ..Account::default()
    }
}
