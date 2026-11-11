use crate::case::{AcctIn, Case};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use revm::{
    database_interface::DBErrorMarker,
    primitives::KECCAK_EMPTY,
    state::{AccountInfo, Bytecode},
    Database,
};
use std::{collections::BTreeMap, str::FromStr};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("the transaction reads the hash of block {0}, which the case does not hold")]
    MissingBlockHash(u64),
}
impl DBErrorMarker for DbError {}

pub fn u256(s: &str) -> U256 {
    U256::from_str(s).unwrap_or_default()
}

pub fn addr(s: &str) -> Address {
    Address::from_str(s).unwrap_or_default()
}

pub fn bytes(s: &str) -> Bytes {
    Bytes::from_str(s).unwrap_or_default()
}

pub fn code_hash(code: &Bytes) -> B256 {
    if code.is_empty() {
        KECCAK_EMPTY
    } else {
        keccak256(code)
    }
}

/// The prestate of one case. Missing accounts and slots read as empty, as in the archive's
/// `prestateTracer`, which drops empty accounts and zero slots.
pub struct CaseDb {
    accounts: BTreeMap<Address, (AccountInfo, BTreeMap<U256, U256>)>,
    hashes: BTreeMap<u64, B256>,
}

impl CaseDb {
    pub fn new(c: &Case) -> Self {
        let accounts = c
            .pre
            .iter()
            .map(|(a, i): (&String, &AcctIn)| {
                let code = bytes(i.code.as_deref().unwrap_or("0x"));
                let info = AccountInfo {
                    balance: u256(i.balance.as_deref().unwrap_or("0x0")),
                    nonce: i.nonce.unwrap_or(0),
                    code_hash: code_hash(&code),
                    code: Some(Bytecode::new_raw(code)),
                    ..Default::default()
                };
                let storage = i.storage.iter().map(|(k, v)| (u256(k), u256(v))).collect();
                (addr(a), (info, storage))
            })
            .collect();
        let hashes = c
            .hashes
            .iter()
            .map(|(n, h)| (*n, B256::from_str(h).unwrap_or_default()))
            .collect();
        Self { accounts, hashes }
    }
}

impl Database for CaseDb {
    type Error = DbError;
    fn basic(&mut self, a: Address) -> Result<Option<AccountInfo>, DbError> {
        Ok(self.accounts.get(&a).map(|(i, _)| i.clone()))
    }
    fn code_by_hash(&mut self, h: B256) -> Result<Bytecode, DbError> {
        Ok(self
            .accounts
            .values()
            .find(|(i, _)| i.code_hash == h)
            .and_then(|(i, _)| i.code.clone())
            .unwrap_or_default())
    }
    fn storage(&mut self, a: Address, k: U256) -> Result<U256, DbError> {
        Ok(self
            .accounts
            .get(&a)
            .and_then(|(_, s)| s.get(&k).copied())
            .unwrap_or_default())
    }
    fn block_hash(&mut self, n: u64) -> Result<B256, DbError> {
        self.hashes
            .get(&n)
            .copied()
            .ok_or(DbError::MissingBlockHash(n))
    }
}
