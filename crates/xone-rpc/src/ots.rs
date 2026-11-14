//! Otterscan `ots_*` namespace, first slice (issue #183): the methods Otterscan needs to show
//! blocks and their transactions, in the shapes of Erigon and reth (`crates/rpc/rpc/src/otterscan.rs`).
//! Served only when the API list names `ots`. The search and internal-operation methods need an
//! address index and a tracer and are not here yet (`-32601`).

use crate::{
    eth::{block_of, state_at},
    json::{block_json, decode_receipts, q, receipt_json_at, Loc},
    server::{Cost, Ctx, Registry},
    types::{BlockRef, Params, RpcError, R},
};
use alloy_primitives::{keccak256, U256};
use serde_json::{json, Value};
use xone_sync::bootstrap::retention::Class;

/// Otterscan's API level that the served methods match (Erigon `ots_getApiLevel`).
pub const API_LEVEL: u64 = 8;
/// The largest page `ots_getBlockTransactions` returns.
const MAX_PAGE: usize = 1_000;

pub fn register(r: &mut Registry) {
    r.add("ots_getApiLevel", Cost::C0, |_, _| Ok(json!(API_LEVEL)));
    r.add("ots_hasCode", Cost::C1, has_code);
    r.add("ots_getBlockDetails", Cost::C2, |c, p| {
        details(c, BlockRef::Number(p.number_or_latest(0)?))
    });
    r.add("ots_getBlockDetailsByHash", Cost::C2, |c, p| {
        details(c, BlockRef::Hash(p.hash(0)?))
    });
    r.add("ots_getBlockTransactions", Cost::C2, block_transactions);
}

fn has_code(c: &Ctx, p: &Params) -> R {
    let a = p.address(0)?;
    let (_, h) = state_at(c, p, 1)?;
    let acct = c.be.state.query(h, |s| s.account(a))?;
    Ok(json!(acct.is_some_and(|x| x.code_hash != keccak256([]))))
}

/// A page index or size: a JSON number or a hex quantity.
fn num(p: &Params, i: usize) -> Result<usize, RpcError> {
    let bad = || RpcError::invalid_params(format!("invalid argument {i}"));
    let n = match p.opt(i) {
        Some(Value::Number(n)) => n.as_u64().ok_or_else(bad)?,
        Some(Value::String(s)) => {
            u64::from_str_radix(s.strip_prefix("0x").ok_or_else(bad)?, 16).map_err(|_| bad())?
        }
        _ => return Err(bad()),
    };
    usize::try_from(n).map_err(|_| bad())
}

/// The block object Otterscan wants: no `transactions`, a `transactionCount`, `logsBloom` null.
fn slim(mut b: Value, count: usize) -> Value {
    if let Value::Object(m) = &mut b {
        m.remove("transactions");
        m.insert("transactionCount".into(), json!(count));
        m.insert("logsBloom".into(), Value::Null);
    }
    b
}

/// Receipts of a block as JSON, and the fee total (gas used times effective price, summed).
fn receipts(c: &Ctx, b: &crate::api::BlockView) -> Result<(Vec<Value>, U256), RpcError> {
    c.be.chain.history(Class::Receipt, b.header.number)?;
    let raw =
        c.be.chain
            .receipts(b.hash)
            .ok_or_else(|| RpcError::server("receipts not found"))?;
    let rc = decode_receipts(&raw)?;
    let id = c.be.chain.spec().chain_id;
    let (mut out, mut fees, mut first_log) = (Vec::with_capacity(b.txs.len()), U256::ZERO, 0usize);
    for (i, t) in b.txs.iter().enumerate() {
        let l = Loc {
            block: b.hash,
            number: b.header.number,
            time: b.header.timestamp,
            index: u64::try_from(i).unwrap_or(u64::MAX),
            base_fee: b.header.base_fee,
        };
        let r = receipt_json_at(t, &rc, i, first_log, &l, id)?;
        first_log += rc.get(i).map_or(0, |r| r.logs.len());
        let field = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix("0x"))
                .and_then(|s| U256::from_str_radix(s, 16).ok())
                .unwrap_or(U256::ZERO)
        };
        fees = fees.saturating_add(field("gasUsed").saturating_mul(field("effectiveGasPrice")));
        out.push(r);
    }
    Ok((out, fees))
}

fn details(c: &Ctx, r: BlockRef) -> R {
    let Some(b) = block_of(c, r)? else {
        return Ok(Value::Null);
    };
    let (_, fees) = receipts(c, &b)?;
    let block = slim(
        block_json(&b, false, c.be.chain.spec().chain_id)?,
        b.txs.len(),
    );
    Ok(json!({
        "block": block,
        // XDPoS pays rewards by system transactions at checkpoints, not by issuance per block;
        // Erigon and reth report zeros where they do not compute it.
        "issuance": {"blockReward": q(0), "uncleReward": q(0), "issuance": q(0)},
        "totalFees": format!("{fees:#x}"),
    }))
}

/// The index range of page `page` of `size` over `len` transactions, counted from the end.
fn page_range(len: usize, page: usize, size: usize) -> (usize, usize) {
    let end = len.saturating_sub(page.saturating_mul(size));
    (end.saturating_sub(size), end)
}

/// `ots_getBlockTransactions(number, pageNumber, pageSize)`: pages count from the end of the block
/// (page 0 holds the last transactions), as Erigon. Inputs are cut to the 4-byte selector; receipts
/// carry no logs and no bloom, and a `timestamp`.
fn block_transactions(c: &Ctx, p: &Params) -> R {
    let n = p.number_or_latest(0)?;
    let (page, size) = (num(p, 1)?, num(p, 2)?);
    if size == 0 || size > MAX_PAGE {
        return Err(RpcError::invalid_params("page size must be 1 to 1000"));
    }
    let Some(b) = block_of(c, BlockRef::Number(n))? else {
        return Ok(Value::Null);
    };
    let (rcs, _) = receipts(c, &b)?;
    let len = b.txs.len();
    let (start, end) = page_range(len, page, size);
    let mut full = block_json(&b, true, c.be.chain.spec().chain_id)?;
    if let Some(Value::Array(txs)) = full.get_mut("transactions") {
        let mut page_txs: Vec<Value> = txs.drain(start..end).collect();
        for t in &mut page_txs {
            if let Some(Value::String(s)) = t.get_mut("input") {
                s.truncate(10);
            }
        }
        *txs = page_txs;
    }
    if let Value::Object(m) = &mut full {
        m.insert("transactionCount".into(), json!(len));
        m.insert("logsBloom".into(), Value::Null);
    }
    let ts = q(b.header.timestamp);
    let receipts: Vec<Value> = rcs[start..end]
        .iter()
        .cloned()
        .map(|mut r| {
            if let Value::Object(m) = &mut r {
                m.insert("logs".into(), Value::Null);
                m.insert("logsBloom".into(), Value::Null);
                m.insert("timestamp".into(), ts.clone());
            }
            r
        })
        .collect();
    Ok(json!({"fullblock": full, "receipts": receipts}))
}

#[cfg(test)]
mod tests {
    use super::page_range;

    #[test]
    fn pages_count_from_the_end() {
        assert_eq!(page_range(25, 0, 10), (15, 25));
        assert_eq!(page_range(25, 1, 10), (5, 15));
        assert_eq!(page_range(25, 2, 10), (0, 5));
        assert_eq!(page_range(25, 3, 10), (0, 0));
        assert_eq!(page_range(0, 0, 10), (0, 0));
    }
}
