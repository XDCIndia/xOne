use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The five eras of the sweep: name and first block, last block exclusive.
pub const ERAS: [(&str, u64, u64); 5] = [
    ("pre-london", 1, 61_290_000),
    ("london-to-eip1559", 61_290_000, 71_550_000),
    ("eip1559-to-v2.9", 71_550_000, 83_600_000),
    ("v2.9-to-prague", 83_600_000, 84_333_000),
    ("post-prague", 84_333_000, u64::MAX),
];

pub fn era_of(number: u64) -> usize {
    ERAS.iter()
        .position(|(_, lo, hi)| (*lo..*hi).contains(&number))
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub name: String,
    /// What the case is a regression for, when it is a named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupling: Option<String>,
    pub network: String,
    pub block: BlockIn,
    /// Coinbase owner at the parent state (`getCandidateOwner`), for blocks after `TIPTRC21Fee`.
    pub owner: Option<String>,
    pub tx: TxIn,
    pub receipt: ReceiptIn,
    pub pre: BTreeMap<String, AcctIn>,
    pub diff_pre: BTreeMap<String, AcctIn>,
    pub diff_post: BTreeMap<String, AcctIn>,
    /// Block hashes the transaction read with BLOCKHASH.
    #[serde(default)]
    pub hashes: BTreeMap<u64, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockIn {
    pub number: u64,
    pub timestamp: u64,
    pub miner: String,
    pub gas_limit: u64,
    pub difficulty: String,
    pub base_fee: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxIn {
    pub hash: String,
    pub index: u64,
    pub ty: u8,
    pub from: String,
    pub to: Option<String>,
    pub input: String,
    pub value: String,
    pub gas: u64,
    pub nonce: u64,
    /// `gasPrice` of a legacy or access-list tx, `maxFeePerGas` of a dynamic-fee tx.
    pub gas_price: String,
    pub max_priority_fee: Option<String>,
    pub access_list: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptIn {
    pub status: u8,
    pub gas_used: u64,
    pub contract_address: Option<String>,
    pub logs: Vec<LogIn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogIn {
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
}

/// One account of a `prestateTracer` result. In a diff every field is present only if it changed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcctIn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub storage: BTreeMap<String, String>,
}
