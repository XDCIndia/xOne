//! Read-only access to the Apothem archive (JSON-RPC over `ssh xdc02 curl`, at most 4 requests per
//! second) and the conversion of its answers into [`Case`]s.

use crate::{
    case::{AcctIn, BlockIn, Case, LogIn, ReceiptIn, TxIn},
    db::{addr, u256},
    replay::{replay, ReplayError},
};
use alloy_primitives::{keccak256, U256};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use xone_chainspec::{ForkBlock, Network};
use xone_primitives::{BlockNonce, Header};

pub const HOST: &str = "xdc02";
pub const APOTHEM_PORT: u16 = 21105;
/// The validator contract (geth `common.MasternodeVotingSMCBinary`).
const VALIDATOR: &str = "0x0000000000000000000000000000000000000088";
/// `slotValidatorMapping["validatorsState"]`, geth `core/state/statedb_utils.go:77`.
const VALIDATORS_STATE_SLOT: u8 = 1;
/// geth `types.IsSpecialTx` (0x89, 0x90) and the XDCx empty transactions (0x91 to 0x94).
const SPECIAL: [&str; 6] = [
    "0x0000000000000000000000000000000000000089",
    "0x0000000000000000000000000000000000000090",
    "0x0000000000000000000000000000000000000091",
    "0x0000000000000000000000000000000000000092",
    "0x0000000000000000000000000000000000000093",
    "0x0000000000000000000000000000000000000094",
];

pub fn is_special(to: &str) -> bool {
    SPECIAL.contains(&to.to_lowercase().as_str())
}

pub struct Rpc {
    last: Instant,
    pub requests: u64,
}

impl Default for Rpc {
    fn default() -> Self {
        Self {
            last: Instant::now() - Duration::from_secs(1),
            requests: 0,
        }
    }
}

impl Rpc {
    /// One HTTP request carrying a JSON-RPC batch; answers in request order.
    pub fn batch(&mut self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>, String> {
        let gap = Duration::from_millis(260);
        if let Some(wait) = gap.checked_sub(self.last.elapsed()) {
            std::thread::sleep(wait);
        }
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (m, p))| json!({"jsonrpc":"2.0","id":i,"method":m,"params":p}))
            .collect();
        let mut child = Command::new("ssh")
            .args([
                HOST,
                "curl",
                "-s",
                "-m",
                "300",
                "-H",
                "'content-type: application/json'",
                "--data-binary",
                "@-",
            ])
            .arg(format!("http://127.0.0.1:{APOTHEM_PORT}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(Value::Array(body).to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        self.last = Instant::now();
        self.requests += 1;
        let v: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("bad answer: {e}"))?;
        let mut answers = vec![Value::Null; calls.len()];
        for a in v.as_array().ok_or("answer is not an array")? {
            if let Some(i) = a["id"].as_u64() {
                answers[i as usize] = a.clone();
            }
        }
        Ok(answers)
    }
}

pub fn hex_u64(v: &Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16).unwrap_or(0)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn acct(v: &Value) -> AcctIn {
    AcctIn {
        balance: v["balance"].as_str().map(str::to_string),
        nonce: v["nonce"].as_u64(),
        code: v["code"].as_str().map(str::to_string),
        storage: v["storage"]
            .as_object()
            .map(|m| m.iter().map(|(k, x)| (k.clone(), s(x))).collect())
            .unwrap_or_default(),
    }
}

fn accts(v: &Value) -> BTreeMap<String, AcctIn> {
    v.as_object()
        .map(|m| m.iter().map(|(k, x)| (k.to_lowercase(), acct(x))).collect())
        .unwrap_or_default()
}

macro_rules! get {
    ($v:expr, $k:expr) => {
        serde_json::from_value($v[$k].clone()).map_err(|e| format!("{}: {e}", $k))?
    };
}

/// The RPC block object as an XDC header (geth `gen_header_json.go`).
fn header_of(v: &Value) -> Result<Header, String> {
    let q = |k: &str| -> Result<u64, String> {
        let x: alloy_primitives::U64 = get!(v, k);
        Ok(x.to())
    };
    Ok(Header {
        parent_hash: get!(v, "parentHash"),
        uncle_hash: get!(v, "sha3Uncles"),
        coinbase: get!(v, "miner"),
        state_root: get!(v, "stateRoot"),
        tx_root: get!(v, "transactionsRoot"),
        receipt_root: get!(v, "receiptsRoot"),
        bloom: get!(v, "logsBloom"),
        difficulty: get!(v, "difficulty"),
        number: q("number")?,
        gas_limit: q("gasLimit")?,
        gas_used: q("gasUsed")?,
        timestamp: q("timestamp")?,
        extra_data: get!(v, "extraData"),
        mix_digest: get!(v, "mixHash"),
        nonce: BlockNonce(get!(v, "nonce")),
        validators: get!(v, "validators"),
        validator: get!(v, "validator"),
        penalties: get!(v, "penalties"),
        base_fee: match v.get("baseFeePerGas") {
            Some(_) => Some(get!(v, "baseFeePerGas")),
            None => None,
        },
        ..Default::default()
    })
}

/// The block's creator, which is geth's coinbase for the block (`engine.Author`, recovered from
/// the seal): the header's `miner` field is zero for V1 blocks.
pub fn author(block: &Value) -> Result<alloy_primitives::Address, String> {
    let h = header_of(block)?;
    let switch = Network::Apothem
        .spec()
        .xdpos
        .v2
        .map_or(u64::MAX, |v| v.switch_block);
    let who = if h.number > switch {
        h.v2_signer()
    } else {
        h.v1_signer()
    };
    who.map_err(|e| format!("block {}: {e}", h.number))
}

/// Why a transaction is not part of the sweep.
pub type Excluded = String;

/// The case of transaction `tx` of `block` (both as returned by `eth_getBlockByNumber`).
pub fn fetch_case(
    rpc: &mut Rpc,
    network: &str,
    block: &Value,
    tx: &Value,
) -> Result<Result<Case, Excluded>, String> {
    let number = hex_u64(&block["number"]);
    let spec = Network::Apothem.spec();
    let hash = s(&tx["hash"]);
    let miner = format!("{:#x}", author(block)?);
    let fee_to_owner = matches!(spec.effective.tip_trc21_fee, ForkBlock::At(b) if number > b);
    let mut calls = vec![
        ("eth_getTransactionReceipt", json!([hash])),
        (
            "debug_traceTransaction",
            json!([hash, {"tracer":"prestateTracer","tracerConfig":{"diffMode":false}}]),
        ),
        (
            "debug_traceTransaction",
            json!([hash, {"tracer":"prestateTracer","tracerConfig":{"diffMode":true}}]),
        ),
    ];
    if fee_to_owner {
        let mut key = [0u8; 64];
        key[12..32].copy_from_slice(addr(&miner).as_slice());
        key[63] = VALIDATORS_STATE_SLOT;
        let slot = format!("{:#x}", keccak256(key));
        calls.push((
            "eth_getStorageAt",
            json!([VALIDATOR, slot, format!("{:#x}", number - 1)]),
        ));
    }
    let a = rpc.batch(calls)?;
    for (i, x) in a.iter().enumerate().take(3) {
        if !x["error"].is_null() || x["result"].is_null() {
            return Ok(Err(format!("archive call {i} failed: {}", x["error"])));
        }
    }
    let (rc, pre, diff) = (&a[0]["result"], &a[1]["result"], &a[2]["result"]);
    let ty = hex_u64(&tx["type"]) as u8;
    if ty >= 3 {
        return Ok(Err(format!(
            "tx type {ty} (blob or set-code) is not covered by xone-evm"
        )));
    }
    if ty >= 1 && !spec.is_eip1559(number) {
        return Ok(Err("access-list or dynamic-fee tx before Eip1559Block: revm rejects the type under Istanbul rules".into()));
    }
    let owner = a.get(3).map(|x| {
        let w = s(&x["result"]);
        format!("0x{}", &w[w.len().saturating_sub(40)..])
    });
    let case = Case {
        name: format!("{network}-{number}-{}", hex_u64(&tx["transactionIndex"])),
        coupling: None,
        network: network.to_string(),
        block: BlockIn {
            number,
            timestamp: hex_u64(&block["timestamp"]),
            miner,
            gas_limit: hex_u64(&block["gasLimit"]),
            difficulty: s(&block["difficulty"]),
            base_fee: block["baseFeePerGas"].as_str().map(str::to_string),
        },
        owner,
        tx: TxIn {
            hash,
            index: hex_u64(&tx["transactionIndex"]),
            ty,
            from: s(&tx["from"]),
            to: tx["to"].as_str().map(str::to_string),
            input: s(&tx["input"]),
            value: s(&tx["value"]),
            gas: hex_u64(&tx["gas"]),
            nonce: hex_u64(&tx["nonce"]),
            gas_price: s(if ty == 2 {
                &tx["maxFeePerGas"]
            } else {
                &tx["gasPrice"]
            }),
            max_priority_fee: tx["maxPriorityFeePerGas"].as_str().map(str::to_string),
            access_list: tx["accessList"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|i| {
                            (
                                s(&i["address"]),
                                i["storageKeys"]
                                    .as_array()
                                    .map(|k| k.iter().map(s).collect())
                                    .unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        receipt: ReceiptIn {
            status: hex_u64(&rc["status"]) as u8,
            gas_used: hex_u64(&rc["gasUsed"]),
            contract_address: rc["contractAddress"].as_str().map(str::to_string),
            logs: rc["logs"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|x| LogIn {
                            address: s(&x["address"]),
                            topics: x["topics"]
                                .as_array()
                                .map(|t| t.iter().map(s).collect())
                                .unwrap_or_default(),
                            data: s(&x["data"]),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        pre: accts(pre),
        diff_pre: accts(&diff["pre"]),
        diff_post: accts(&diff["post"]),
        hashes: BTreeMap::new(),
    };
    // The sender of a TRC21 token-fee transaction pays no gas from its balance; XB-08 owns that path.
    let from = case.tx.from.to_lowercase();
    let pre_b = case
        .pre
        .get(&from)
        .and_then(|x| x.balance.as_deref())
        .map_or(U256::ZERO, u256);
    let post_b = case
        .diff_post
        .get(&from)
        .and_then(|x| x.balance.as_deref())
        .map_or(pre_b, u256);
    if case.tx.to.is_some()
        && pre_b == post_b.saturating_add(u256(&case.tx.value))
        && case.receipt.gas_used > 0
        && u256(&case.tx.gas_price) > U256::ZERO
    {
        return Ok(Err(
            "TRC21 token-fee tx (sender pays no gas): fee path is XB-08".into(),
        ));
    }
    if let Some(to) = &case.tx.to {
        let has_code = case
            .pre
            .get(&to.to_lowercase())
            .and_then(|x| x.code.as_deref())
            .is_some_and(|c| c.len() > 2);
        if !has_code {
            return Ok(Err("recipient has no code: not a contract call".into()));
        }
    }
    Ok(Ok(with_hashes(rpc, case)?))
}

/// Fetch the hash of every block the transaction reads with BLOCKHASH.
fn with_hashes(rpc: &mut Rpc, mut c: Case) -> Result<Case, String> {
    for _ in 0..300 {
        match replay(&c) {
            Err(ReplayError::MissingBlockHash(n)) => {
                let a = rpc.batch(vec![(
                    "eth_getBlockByNumber",
                    json!([format!("{n:#x}"), false]),
                )])?;
                c.hashes.insert(n, s(&a[0]["result"]["hash"]));
            }
            _ => break,
        }
    }
    Ok(c)
}
