//! geth's JSON for headers, blocks, transactions, receipts and logs (`internal/ethapi/api.go`:
//! `RPCMarshalHeader` 996, `RPCMarshalBlock` 1048, `newRPCTransaction` 1110, `MarshalReceipt` 1612).

use crate::{api::BlockView, types::RpcError};
use alloy_primitives::{keccak256, logs_bloom, Address, Bytes, Log, B256, U256};
use alloy_rlp::Encodable;
use serde_json::{json, Map, Value};
use xone_consensus::verify::Receipt;
use xone_executor::tx;
use xone_primitives::Header;

/// `hexutil.Uint64` and `hexutil.Big`: no leading zeros, `0x0` for zero.
pub fn q(n: u64) -> Value {
    Value::String(format!("{n:#x}"))
}
pub fn qu(n: U256) -> Value {
    Value::String(format!("{n:#x}"))
}
/// `hexutil.Bytes`.
pub fn hex(b: &[u8]) -> Value {
    Value::String(format!("0x{}", alloy_primitives::hex::encode(b)))
}
pub fn h256(h: B256) -> Value {
    Value::String(format!("{h:#x}"))
}
/// A lower-case address, as `common.Address.MarshalText` writes it.
pub fn addr(a: Address) -> Value {
    Value::String(format!("{a:#x}"))
}

/// `RPCMarshalHeader`: the XDPoS fields `validators`, `validator` and `penalties` are always present.
pub fn header_json(h: &Header, hash: B256) -> Map<String, Value> {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put("number", q(h.number));
    put("hash", h256(hash));
    put("parentHash", h256(h.parent_hash));
    put("nonce", hex(h.nonce.0.as_slice()));
    put("mixHash", h256(h.mix_digest));
    put("sha3Uncles", h256(h.uncle_hash));
    put("logsBloom", hex(h.bloom.as_slice()));
    put("stateRoot", h256(h.state_root));
    put("miner", addr(h.coinbase));
    put("difficulty", qu(h.difficulty));
    put("extraData", hex(&h.extra_data));
    put("gasLimit", q(h.gas_limit));
    put("gasUsed", q(h.gas_used));
    put("timestamp", q(h.timestamp));
    put("transactionsRoot", h256(h.tx_root));
    put("receiptsRoot", h256(h.receipt_root));
    put("validators", hex(&h.validators));
    put("validator", hex(&h.validator));
    put("penalties", hex(&h.penalties));
    if let Some(v) = h.base_fee {
        put("baseFeePerGas", qu(v));
    }
    if let Some(v) = h.withdrawals_hash {
        put("withdrawalsRoot", h256(v));
    }
    if let Some(v) = h.blob_gas_used {
        put("blobGasUsed", q(v));
    }
    if let Some(v) = h.excess_blob_gas {
        put("excessBlobGas", q(v));
    }
    if let Some(v) = h.parent_beacon_root {
        put("parentBeaconBlockRoot", h256(v));
    }
    if let Some(v) = h.requests_hash {
        put("requestsHash", h256(v));
    }
    if let Some(v) = h.block_access_list_hash {
        put("blockAccessListHash", h256(v));
    }
    if let Some(v) = h.slot_number {
        put("slotNumber", q(v));
    }
    m
}

/// `Header.Hash()` of the uncles, for the `uncles` field.
fn uncle_hashes(b: &BlockView) -> Value {
    Value::Array(b.uncles.iter().map(|u| h256(u.hash())).collect())
}

/// `RPCMarshalBlock` with `inclTx` true. A transaction that does not decode is an error (the body
/// was verified on import, so this is a corrupt store).
pub fn block_json(b: &BlockView, full: bool, chain_id: u64) -> Result<Value, RpcError> {
    let mut m = header_json(&b.header, b.hash);
    m.insert(
        "size".into(),
        q(u64::try_from(b.raw.len()).unwrap_or(u64::MAX)),
    );
    let mut txs = Vec::with_capacity(b.txs.len());
    for (i, raw) in b.txs.iter().enumerate() {
        txs.push(if full {
            let loc = Loc {
                block: b.hash,
                number: b.header.number,
                time: b.header.timestamp,
                index: u64::try_from(i).unwrap_or(u64::MAX),
                base_fee: b.header.base_fee,
            };
            tx_json(raw, chain_id, Some(&loc))?
        } else {
            h256(keccak256(raw))
        });
    }
    m.insert("transactions".into(), Value::Array(txs));
    m.insert("uncles".into(), uncle_hashes(b));
    // A post-Shanghai block carries a withdrawals list; on XDC it is always empty (net2040 vectors:
    // 47 of 47 post-Shanghai blocks `"withdrawals": []`). ponytail: a real list when a chain pays one.
    if b.header.withdrawals_hash.is_some() {
        m.insert("withdrawals".into(), json!([]));
    }
    Ok(Value::Object(m))
}

/// Where a mined transaction sits.
pub struct Loc {
    pub block: B256,
    pub number: u64,
    pub time: u64,
    pub index: u64,
    pub base_fee: Option<U256>,
}

/// The last three items of an RLP list: `v` (or `yParity`), `r`, `s`, as big endian integers.
fn signature(raw: &[u8]) -> Option<[U256; 3]> {
    let mut b = match raw.first()? {
        0xc0.. => raw,
        _ => &raw[1..],
    };
    let list = alloy_rlp::Header::decode(&mut b).ok()?;
    if !list.list || list.payload_length > b.len() {
        return None;
    }
    let mut p = &b[..list.payload_length];
    let mut items = Vec::new();
    while !p.is_empty() {
        let before = p;
        let h = alloy_rlp::Header::decode(&mut p).ok()?;
        if h.payload_length > p.len() {
            return None;
        }
        let payload = &p[..h.payload_length];
        p = &p[h.payload_length..];
        items.push(if h.list { before } else { payload });
    }
    let n = items.len();
    let big = |x: &[u8]| U256::try_from_be_slice(x);
    Some([
        big(items.get(n.checked_sub(3)?)?)?,
        big(items.get(n.checked_sub(2)?)?)?,
        big(items.get(n.checked_sub(1)?)?)?,
    ])
}

/// `newRPCTransaction`. `loc` is `None` for a pool transaction.
pub fn tx_json(raw: &[u8], chain_id: u64, loc: Option<&Loc>) -> Result<Value, RpcError> {
    let t = tx::decode(raw, chain_id).map_err(|e| RpcError::internal(e.to_string()))?;
    let [v, r, s] = signature(raw).ok_or_else(|| RpcError::internal("transaction signature"))?;
    let mut m = Map::new();
    let mut put = |k: &str, x: Value| {
        m.insert(k.to_owned(), x);
    };
    let fee_cap = U256::from(t.gas_price);
    let price = match (t.ty, loc.and_then(|l| l.base_fee)) {
        // `effectiveGasPrice`: min(tip + baseFee, feeCap) once mined with a base fee.
        (2, Some(base)) => {
            let tip = U256::from(t.priority_fee.unwrap_or(t.gas_price));
            (tip + base).min(fee_cap)
        }
        _ => fee_cap,
    };
    put("type", q(u64::from(t.ty)));
    put("from", addr(t.from));
    put("gas", q(t.gas));
    put("gasPrice", qu(price));
    put("hash", h256(keccak256(raw)));
    put("input", hex(&t.data));
    put("nonce", q(t.nonce));
    put("to", t.to.map_or(Value::Null, addr));
    put("value", qu(t.value));
    put("v", qu(v));
    put("r", qu(r));
    put("s", qu(s));
    match loc {
        Some(l) => {
            put("blockHash", h256(l.block));
            put("blockNumber", q(l.number));
            put("blockTimestamp", q(l.time));
            put("transactionIndex", q(l.index));
        }
        None => {
            put("blockHash", Value::Null);
            put("blockNumber", Value::Null);
            put("blockTimestamp", Value::Null);
            put("transactionIndex", Value::Null);
        }
    }
    if t.ty == 0 {
        if let Some(id) = t.chain_id.filter(|c| *c != 0) {
            put("chainId", q(id));
        }
    } else {
        put("chainId", q(t.chain_id.unwrap_or(chain_id)));
        put("yParity", q(u64::from(!v.is_zero())));
        put(
            "accessList",
            Value::Array(
                t.access_list
                    .iter()
                    .map(|a| json!({"address": addr(a.address), "storageKeys": a.keys.iter().map(|k| h256(*k)).collect::<Vec<_>>()}))
                    .collect(),
            ),
        );
    }
    if t.ty == 2 {
        put("maxFeePerGas", qu(fee_cap));
        put(
            "maxPriorityFeePerGas",
            qu(U256::from(t.priority_fee.unwrap_or(0))),
        );
    }
    Ok(Value::Object(m))
}

/// One log as `types.Log.MarshalJSON` writes it.
pub fn log_json(
    l: &Log,
    block: &Loc,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
    removed: bool,
) -> Value {
    json!({
        "address": addr(l.address),
        "topics": l.data.topics().iter().map(|t| h256(*t)).collect::<Vec<_>>(),
        "data": hex(&l.data.data),
        "blockNumber": q(block.number),
        "transactionHash": h256(tx_hash),
        "transactionIndex": q(tx_index),
        "blockHash": h256(block.block),
        "blockTimestamp": q(block.time),
        "logIndex": q(log_index),
        "removed": removed,
    })
}

pub fn decode_receipts(raw: &[Bytes]) -> Result<Vec<Receipt>, RpcError> {
    raw.iter()
        .map(|r: &Bytes| Receipt::decode(r).map_err(|e| RpcError::internal(e.to_string())))
        .collect()
}

/// `MarshalReceipt` for transaction `i` of the block. `receipts` are all the block's receipts.
pub fn receipt_json(
    raw_tx: &[u8],
    receipts: &[Receipt],
    i: usize,
    loc: &Loc,
    chain_id: u64,
) -> Result<Value, RpcError> {
    let t = tx::decode(raw_tx, chain_id).map_err(|e| RpcError::internal(e.to_string()))?;
    let rc = receipts
        .get(i)
        .ok_or_else(|| RpcError::internal("receipt missing"))?;
    let before = if i == 0 {
        0
    } else {
        receipts[i - 1].cumulative_gas_used
    };
    let first_log: usize = receipts[..i].iter().map(|r| r.logs.len()).sum();
    let tx_hash = keccak256(raw_tx);
    let fee_cap = U256::from(t.gas_price);
    let eff = match (t.ty, loc.base_fee) {
        (2, Some(base)) => (U256::from(t.priority_fee.unwrap_or(t.gas_price)) + base).min(fee_cap),
        _ => fee_cap,
    };
    let logs: Vec<Value> = rc
        .logs
        .iter()
        .enumerate()
        .map(|(j, l)| {
            log_json(
                l,
                loc,
                tx_hash,
                loc.index,
                u64::try_from(first_log + j).unwrap_or(u64::MAX),
                false,
            )
        })
        .collect();
    let mut m = Map::new();
    m.insert("blockHash".into(), h256(loc.block));
    m.insert("blockNumber".into(), q(loc.number));
    m.insert("transactionHash".into(), h256(tx_hash));
    m.insert("transactionIndex".into(), q(loc.index));
    m.insert("from".into(), addr(t.from));
    m.insert("to".into(), t.to.map_or(Value::Null, addr));
    m.insert(
        "gasUsed".into(),
        q(rc.cumulative_gas_used.saturating_sub(before)),
    );
    m.insert("cumulativeGasUsed".into(), q(rc.cumulative_gas_used));
    m.insert("logs".into(), Value::Array(logs));
    m.insert(
        "logsBloom".into(),
        hex(logs_bloom(rc.logs.iter()).as_slice()),
    );
    m.insert("type".into(), q(u64::from(t.ty)));
    m.insert("effectiveGasPrice".into(), qu(eff));
    let ps = rc.post_state_or_status.as_ref();
    if ps.len() > 1 {
        m.insert("root".into(), hex(ps));
    } else {
        m.insert("status".into(), q(u64::from(ps == [1])));
    }
    m.insert(
        "contractAddress".into(),
        if t.to.is_none() {
            addr(t.from.create(t.nonce))
        } else {
            Value::Null
        },
    );
    Ok(Value::Object(m))
}

/// The RLP of a header (`debug_getRawHeader`).
pub fn header_rlp(h: &Header) -> Vec<u8> {
    let mut out = Vec::new();
    h.encode(&mut out);
    out
}

/// Standard base64 with padding (`encoding/base64.StdEncoding`).
pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, x)| a | u32::from(*x) << (16 - 8 * i));
        for k in 0..4 {
            if k <= c.len() {
                out.push(char::from(
                    T[usize::try_from(n >> (18 - 6 * k) & 63).unwrap_or(0)],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A `*big.Int` as geth writes it: a JSON number, however large. `serde_json::Value` cannot hold a
/// number above `u64` (its `arbitrary_precision` feature would change every crate that shares the
/// build), so a large value is a marked string that [`render`] rewrites to the bare digits.
pub fn bigint(v: U256) -> Value {
    u64::try_from(v).map_or_else(|_| Value::String(format!("\u{1}{v}")), Value::from)
}

/// `serde_json::to_vec` plus the [`bigint`] rewrite.
pub fn render(v: &Value) -> Vec<u8> {
    let raw = serde_json::to_vec(v).unwrap_or_else(|_| b"null".to_vec());
    const MARK: &[u8] = b"\"\\u0001";
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with(MARK) {
            if let Some(end) = raw[i + MARK.len()..].iter().position(|b| *b == b'"') {
                out.extend_from_slice(&raw[i + MARK.len()..i + MARK.len() + end]);
                i += MARK.len() + end + 1;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn quantities() {
        assert_eq!(q(0), "0x0");
        assert_eq!(q(255), "0xff");
        assert_eq!(qu(U256::from(4096)), "0x1000");
    }

    #[test]
    fn header_has_xdpos_fields() {
        let h = Header::default();
        let m = header_json(&h, h.hash());
        for k in ["validators", "validator", "penalties"] {
            assert!(m.contains_key(k), "{k}");
        }
        assert!(!m.contains_key("baseFeePerGas"));
    }
}
