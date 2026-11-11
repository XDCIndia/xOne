#![allow(dead_code)]
use alloy_primitives::{hex, Address, B256, U256};
use std::str::FromStr;
use xone_primitives::WideBalance;

pub fn bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_string()
    };
    hex::decode(s).unwrap()
}

/// Quantity as the oracle prints it (`big.Int.Text(16)`): any width.
pub fn wide(s: &str) -> WideBalance {
    WideBalance::from_be_bytes(&bytes(s))
}

pub fn addr(s: &str) -> Address {
    Address::from_str(&s.to_lowercase()).unwrap()
}

pub fn b256(s: &str) -> B256 {
    B256::from_str(s).unwrap()
}

pub fn word(s: &str) -> U256 {
    U256::from_be_slice(&bytes(s))
}
