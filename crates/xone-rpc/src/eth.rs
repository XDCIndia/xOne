//! The `eth` namespace subset the fleet uses (geth `internal/ethapi/api.go`, `eth/filters/api.go`).
//! Out of scope, `-32601`: `eth_getProof` (spec/rpc/scope-adr-008.md).

use crate::{
    api::{BlockView, CallRequest},
    json::{
        addr, block_json, decode_receipts, h256, hex, log_json, q, qu, receipt_json, tx_json, Loc,
    },
    server::{Cost, Ctx, Registry},
    subscriptions::LogFilter,
    types::{parse_address, parse_tag, BlockRef, Params, RpcError, R},
};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use xone_sync::bootstrap::retention::Class;

pub fn register(r: &mut Registry) {
    r.add("eth_chainId", Cost::C0, |c, _| {
        Ok(q(c.be.chain.spec().chain_id))
    });
    r.add("eth_blockNumber", Cost::C0, |c, _| {
        Ok(q(c.be.chain.head().0))
    });
    r.add("eth_syncing", Cost::C0, |c, _| {
        Ok(match c.be.chain.syncing() {
            None => json!(false),
            Some((s, cur, hi)) => {
                json!({"startingBlock": q(s), "currentBlock": q(cur), "highestBlock": q(hi)})
            }
        })
    });
    r.add("eth_mining", Cost::C0, |c, _| {
        Ok(json!(c.be.chain.mining_as().is_some()))
    });
    r.add("eth_coinbase", Cost::C0, |c, _| {
        c.be.chain
            .mining_as()
            .map(addr)
            .ok_or_else(|| RpcError::server("etherbase must be explicitly specified"))
    });
    r.add("eth_gasPrice", Cost::C0, |c, _| {
        Ok(qu(c.be.pool.gas_price()))
    });
    r.add("eth_feeHistory", Cost::C2, fee_history);
    r.add("eth_getBlockByNumber", Cost::C1, |c, p| {
        block_by(c, p, true)
    });
    r.add("eth_getBlockByHash", Cost::C1, |c, p| block_by(c, p, false));
    r.add("eth_getBlockTransactionCountByNumber", Cost::C1, |c, p| {
        count(c, p, true)
    });
    r.add("eth_getBlockTransactionCountByHash", Cost::C1, |c, p| {
        count(c, p, false)
    });
    r.add("eth_getUncleCountByBlockNumber", Cost::C1, |c, p| {
        uncles(c, p, true)
    });
    r.add("eth_getUncleCountByBlockHash", Cost::C1, |c, p| {
        uncles(c, p, false)
    });
    r.add("eth_getBlockReceipts", Cost::C2, block_receipts);
    r.add("eth_getTransactionByHash", Cost::C1, tx_by_hash);
    r.add(
        "eth_getTransactionByBlockNumberAndIndex",
        Cost::C1,
        |c, p| tx_by_index(c, p, true),
    );
    r.add("eth_getTransactionByBlockHashAndIndex", Cost::C1, |c, p| {
        tx_by_index(c, p, false)
    });
    r.add("eth_getTransactionReceipt", Cost::C1, tx_receipt);
    r.add("eth_getBalance", Cost::C1, balance);
    r.add("eth_getTransactionCount", Cost::C1, tx_count);
    r.add("eth_getCode", Cost::C1, code);
    r.add("eth_getStorageAt", Cost::C1, storage);
    r.add("eth_call", Cost::C1, call);
    r.add("eth_estimateGas", Cost::C2, estimate);
    r.add("eth_getLogs", Cost::C2, get_logs);
    r.add("eth_sendRawTransaction", Cost::C1, send_raw);
}

/// geth `errors.New("header not found")` for a state read at an unknown block.
fn no_header() -> RpcError {
    RpcError::server("header not found")
}

/// The block of a selector: canonical by number (above the head is `None`), or by hash.
pub fn block_of(c: &Ctx, r: BlockRef) -> Result<Option<BlockView>, RpcError> {
    let hash = match r {
        BlockRef::Hash(h) => h,
        BlockRef::Number(n) => {
            if n > c.be.chain.head().0 {
                return Ok(None);
            }
            c.be.chain.history(Class::Body, n)?;
            match c.be.chain.hash_by_number(n) {
                Some(h) => h,
                None => return Ok(None),
            }
        }
    };
    Ok(c.be.chain.block(hash))
}

/// The header a state read runs at; unknown is "header not found", below the window is the typed error.
fn state_at(c: &Ctx, p: &Params, i: usize) -> Result<(u64, B256), RpcError> {
    let r = p.block_ref_or_latest(i)?;
    let hash = match r {
        BlockRef::Hash(h) => h,
        BlockRef::Number(n) => c.be.chain.hash_by_number(n).ok_or_else(no_header)?,
    };
    let h = c.be.chain.header(hash).ok_or_else(no_header)?;
    c.be.chain.history(Class::State, h.number)?;
    Ok((h.number, hash))
}

fn block_by(c: &Ctx, p: &Params, by_number: bool) -> R {
    let r = if by_number {
        BlockRef::Number(p.number_or_latest(0)?)
    } else {
        BlockRef::Hash(p.hash(0)?)
    };
    match block_of(c, r)? {
        Some(b) => block_json(&b, p.bool_or(1, false)?, c.be.chain.spec().chain_id),
        None => Ok(Value::Null),
    }
}

fn selector(p: &Params, by_number: bool) -> Result<BlockRef, RpcError> {
    Ok(if by_number {
        BlockRef::Number(p.number_or_latest(0)?)
    } else {
        BlockRef::Hash(p.hash(0)?)
    })
}

fn count(c: &Ctx, p: &Params, by_number: bool) -> R {
    Ok(
        block_of(c, selector(p, by_number)?)?.map_or(Value::Null, |b| {
            q(u64::try_from(b.txs.len()).unwrap_or(u64::MAX))
        }),
    )
}

fn uncles(c: &Ctx, p: &Params, by_number: bool) -> R {
    Ok(
        block_of(c, selector(p, by_number)?)?.map_or(Value::Null, |b| {
            q(u64::try_from(b.uncles.len()).unwrap_or(u64::MAX))
        }),
    )
}

fn loc(b: &BlockView, index: usize) -> Loc {
    Loc {
        block: b.hash,
        number: b.header.number,
        time: b.header.timestamp,
        index: u64::try_from(index).unwrap_or(u64::MAX),
        base_fee: b.header.base_fee,
    }
}

fn block_receipts(c: &Ctx, p: &Params) -> R {
    let r = p.block_ref_or_latest(0)?;
    let Some(b) = block_of(c, r)? else {
        return Ok(Value::Null);
    };
    c.be.chain.history(Class::Receipt, b.header.number)?;
    let raw =
        c.be.chain
            .receipts(b.hash)
            .ok_or_else(|| RpcError::server("receipts not found"))?;
    let rc = decode_receipts(&raw)?;
    let id = c.be.chain.spec().chain_id;
    let mut out = Vec::with_capacity(b.txs.len());
    for (i, t) in b.txs.iter().enumerate() {
        out.push(receipt_json(t, &rc, i, &loc(&b, i), id)?);
    }
    Ok(Value::Array(out))
}

fn tx_at(c: &Ctx, b: &BlockView, i: usize) -> R {
    match b.txs.get(i) {
        Some(t) => tx_json(t, c.be.chain.spec().chain_id, Some(&loc(b, i))),
        None => Ok(Value::Null),
    }
}

fn index(p: &Params, i: usize) -> Result<usize, RpcError> {
    usize::try_from(parse_tag(p.str(i)?, 0)?).map_err(|_| RpcError::invalid_params("invalid index"))
}

fn tx_by_index(c: &Ctx, p: &Params, by_number: bool) -> R {
    let r = if by_number {
        BlockRef::Number(p.number_or_latest(0)?)
    } else {
        BlockRef::Hash(p.hash(0)?)
    };
    let i = index(p, 1)?;
    match block_of(c, r)? {
        Some(b) => tx_at(c, &b, i),
        None => Ok(Value::Null),
    }
}

fn tx_by_hash(c: &Ctx, p: &Params) -> R {
    let h = p.hash(0)?;
    if let Some((bh, i)) = c.be.chain.tx_location(h) {
        if let Some(b) = c.be.chain.block(bh) {
            return tx_at(c, &b, usize::try_from(i).unwrap_or(usize::MAX));
        }
    }
    // Not mined: the pool (geth `GetPoolTransaction`).
    let (pending, queued) = c.be.pool.content();
    for raw in pending.values().chain(queued.values()).flatten() {
        if keccak256(raw) == h {
            return tx_json(raw, c.be.chain.spec().chain_id, None);
        }
    }
    Ok(Value::Null)
}

fn tx_receipt(c: &Ctx, p: &Params) -> R {
    let Some((bh, i)) = c.be.chain.tx_location(p.hash(0)?) else {
        return Ok(Value::Null);
    };
    let b =
        c.be.chain
            .block(bh)
            .ok_or_else(|| RpcError::server("block not found"))?;
    c.be.chain.history(Class::Receipt, b.header.number)?;
    let raw =
        c.be.chain
            .receipts(bh)
            .ok_or_else(|| RpcError::server("receipts not found"))?;
    let i = usize::try_from(i).unwrap_or(usize::MAX);
    let tx = b
        .txs
        .get(i)
        .ok_or_else(|| RpcError::server("transaction not found"))?;
    receipt_json(
        tx,
        &decode_receipts(&raw)?,
        i,
        &loc(&b, i),
        c.be.chain.spec().chain_id,
    )
}

fn balance(c: &Ctx, p: &Params) -> R {
    let a = p.address(0)?;
    let (_, h) = state_at(c, p, 1)?;
    // A balance above 2^256 (the wide-balance rows of XB-01) has no `hexutil.Big` limit in geth;
    // `to_u256` is `None` only then, and the answer is the low word's truth, so refuse instead.
    let bal =
        c.be.state
            .query(h, |s| s.account(a))?
            .map(|x| x.balance.to_u256());
    match bal {
        None => Ok(q(0)),
        Some(Some(v)) => Ok(qu(v)),
        Some(None) => Err(RpcError::server("balance does not fit 256 bits")),
    }
}

fn tx_count(c: &Ctx, p: &Params) -> R {
    let a = p.address(0)?;
    if matches!(p.opt(1).and_then(Value::as_str), Some("pending")) {
        if let Some(n) = c.be.pool.pending_nonce(a) {
            return Ok(q(n));
        }
    }
    let (_, h) = state_at(c, p, 1)?;
    Ok(q(c
        .be
        .state
        .query(h, |s| s.account(a))?
        .map_or(0, |x| x.nonce)))
}

fn code(c: &Ctx, p: &Params) -> R {
    let a = p.address(0)?;
    let (_, h) = state_at(c, p, 1)?;
    let Some(acct) = c.be.state.query(h, |s| s.account(a))? else {
        return Ok(hex(&[]));
    };
    if acct.code_hash == keccak256([]) {
        return Ok(hex(&[]));
    }
    c.be.state
        .code(acct.code_hash)
        .map(|b| hex(&b))
        .ok_or_else(|| RpcError::server("code not found"))
}

fn storage(c: &Ctx, p: &Params) -> R {
    let a = p.address(0)?;
    // geth `hexutil.Big` key: any length up to 32 bytes, left padded.
    let key = p.str(1)?;
    let k = U256::from_str_radix(
        key.strip_prefix("0x")
            .ok_or_else(|| RpcError::invalid_params("invalid storage key"))?,
        16,
    )
    .map_err(|_| RpcError::invalid_params("invalid storage key"))?;
    let (_, h) = state_at(c, p, 2)?;
    let v = c.be.state.query(h, |s| s.storage(a, B256::from(k)))?;
    Ok(h256(B256::from(v)))
}

fn call_request(o: &serde_json::Map<String, Value>, cap: u64) -> Result<CallRequest, RpcError> {
    let big = |k: &str| -> Result<Option<U256>, RpcError> {
        match o.get(k).and_then(Value::as_str) {
            None => Ok(None),
            Some(s) => U256::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
                .map(Some)
                .map_err(|_| RpcError::invalid_params(format!("invalid {k}"))),
        }
    };
    let ad = |k: &str| -> Result<Option<Address>, RpcError> {
        o.get(k)
            .and_then(Value::as_str)
            .map(parse_address)
            .transpose()
    };
    let gas = match o.get("gas").and_then(Value::as_str) {
        Some(s) => parse_tag(s, 0)?.min(cap),
        None => cap,
    };
    let data = o
        .get("input")
        .or_else(|| o.get("data"))
        .and_then(Value::as_str);
    Ok(CallRequest {
        from: ad("from")?,
        to: ad("to")?,
        gas: Some(gas),
        gas_price: big("gasPrice")?,
        max_fee: big("maxFeePerGas")?,
        max_priority_fee: big("maxPriorityFeePerGas")?,
        value: big("value")?,
        data: match data {
            Some(s) => Bytes::from(
                alloy_primitives::hex::decode(s)
                    .map_err(|_| RpcError::invalid_params("invalid data"))?,
            ),
            None => Bytes::new(),
        },
    })
}

fn call(c: &Ctx, p: &Params) -> R {
    let req = call_request(p.obj(0)?, c.limits.call_gas_cap)?;
    let (_, h) = state_at(c, p, 1)?;
    let out = c.be.state.call(h, &req, c.cancel)?;
    match out.error {
        None => Ok(hex(&out.output)),
        // geth `revertError`: code 3, the reason in the message and the output in `data`.
        Some(e) if e.starts_with("execution reverted") => Err(RpcError {
            code: 3,
            message: e,
            data: Some(hex(&out.output)),
        }),
        Some(e) => Err(RpcError::server(e)),
    }
}

fn estimate(c: &Ctx, p: &Params) -> R {
    let req = call_request(p.obj(0)?, c.limits.call_gas_cap)?;
    let (_, h) = state_at(c, p, 1)?;
    Ok(q(c.be.state.estimate_gas(h, &req, c.cancel)?))
}

fn send_raw(c: &Ctx, p: &Params) -> R {
    let s = p.str(0)?;
    let raw = alloy_primitives::hex::decode(s).map_err(|_| {
        RpcError::invalid_params("invalid argument 0: json: cannot unmarshal hex string")
    })?;
    c.be.pool.add_raw(&raw).map(h256).map_err(RpcError::server)
}

fn get_logs(c: &Ctx, p: &Params) -> R {
    let o = p.obj(0)?;
    let filter = LogFilter::parse(o)?;
    let head = c.be.chain.head().0;
    let blocks: Vec<B256> = if let Some(h) = o.get("blockHash").and_then(Value::as_str) {
        vec![crate::types::parse_hash(h)?]
    } else {
        let end = |k: &str, d: u64| match o.get(k).and_then(Value::as_str) {
            None => Ok(d),
            Some(s) => parse_tag(s, head),
        };
        let (from, to) = (end("fromBlock", head)?, end("toBlock", head)?);
        if from > to {
            return Err(RpcError::invalid_params("invalid block range"));
        }
        if to - from >= c.limits.logs_blocks {
            return Err(RpcError::invalid_params(format!(
                "exceed maximum block range: {}",
                c.limits.logs_blocks
            )));
        }
        c.be.chain.history(Class::Receipt, from)?;
        (from..=to.min(head))
            .filter_map(|n| c.be.chain.hash_by_number(n))
            .collect()
    };
    let id = c.be.chain.spec().chain_id;
    let mut out = Vec::new();
    for bh in blocks {
        if c.cancel.load(Ordering::Relaxed) {
            return Err(RpcError::server("request cancelled"));
        }
        let (Some(b), Some(raw)) = (c.be.chain.block(bh), c.be.chain.receipts(bh)) else {
            continue;
        };
        let rc = decode_receipts(&raw)?;
        let mut idx = 0u64;
        for (i, r) in rc.iter().enumerate() {
            let l = loc(&b, i);
            let tx = b
                .txs
                .get(i)
                .map(keccak256)
                .ok_or_else(|| RpcError::internal("receipt without transaction"))?;
            for lg in &r.logs {
                if filter.matches(lg.address, lg.data.topics()) {
                    out.push(log_json(lg, &l, tx, l.index, idx, false));
                }
                idx += 1;
            }
        }
        let _ = id;
        if out.len() > c.limits.logs_results {
            return Err(RpcError::server(format!(
                "query returned more than {} results",
                c.limits.logs_results
            )));
        }
    }
    Ok(Value::Array(out))
}

/// `eth_feeHistory` (geth `eth/gasprice/feehistory.go`): `baseFeePerGas` has one more entry than the
/// range, the last being the next block's base fee, which xOne does not predict: it repeats the
/// last block's value (ponytail: exact next-block base fee when the fee rules are exposed).
/// geth `maxHeaderHistory` and `maxBlockHistory` defaults (`eth/ethconfig/config.go`).
const FEE_HISTORY_MAX_BLOCKS: u64 = 1024;
/// geth `maxQueryLimit`, `eth/gasprice/feehistory.go:47`.
const FEE_HISTORY_MAX_PERCENTILES: usize = 100;

/// geth `float64(x)`.
#[allow(clippy::as_conversions)] // geth's own float arithmetic, reproduced
fn f64_of(x: u64) -> f64 {
    x as f64
}

/// geth `uint64(f)` for a finite, non-negative `f` (truncation).
#[allow(clippy::as_conversions)] // geth's own float arithmetic, reproduced
fn u64_of(f: f64) -> u64 {
    f as u64
}

/// geth `EffectiveGasTip` (`core/types/transaction.go:384-415`): the tip cap without a base fee;
/// below the base fee the fee cap itself (geth leaves `dst` at the cap on `ErrGasFeeCapTooLow`);
/// else `min(tip cap, fee cap - base fee)`.
fn effective_tip(fee_cap: u128, tip_cap: u128, base: Option<U256>) -> U256 {
    let (cap, tip) = (U256::from(fee_cap), U256::from(tip_cap));
    match base {
        None => tip,
        Some(b) if cap < b => cap,
        Some(b) => tip.min(cap - b),
    }
}

/// geth `processBlock` reward row (`eth/gasprice/feehistory.go:116-150`): `(tip, gas used)` per
/// transaction, sorted stably by tip, walked against `gasUsed * p / 100` of the block.
fn reward_row(block_gas_used: u64, mut txs: Vec<(U256, u64)>, pct: &[f64]) -> Vec<Value> {
    if txs.is_empty() {
        return pct.iter().map(|_| q(0)).collect();
    }
    txs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut idx = 0usize;
    let mut sum = txs.first().map_or(0, |t| t.1);
    pct.iter()
        .map(|p| {
            let threshold = u64_of(f64_of(block_gas_used) * p / 100.0);
            while sum < threshold && idx + 1 < txs.len() {
                idx += 1;
                sum = sum.saturating_add(txs.get(idx).map_or(0, |t| t.1));
            }
            qu(txs.get(idx).map_or(U256::ZERO, |t| t.0))
        })
        .collect()
}

/// geth `eth_feeHistory` (`internal/ethapi/api.go:107-142`, `eth/gasprice/feehistory.go:239-361`).
fn fee_history(c: &Ctx, p: &Params) -> R {
    // geth `math.HexOrDecimal64`: a hex or decimal string, or a JSON number
    let bad = || RpcError::invalid_params("invalid argument 0: invalid block count");
    let count = match p.opt(0) {
        Some(Value::String(s)) => match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(h) => u64::from_str_radix(h, 16).map_err(|_| bad())?,
            None => s.parse::<u64>().map_err(|_| bad())?,
        },
        Some(Value::Number(n)) => n.as_u64().ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    let pct: Vec<f64> = match p.opt(2) {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_f64()
                    .ok_or_else(|| RpcError::invalid_params("invalid reward percentile"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(RpcError::invalid_params("invalid reward percentiles")),
    };
    if count == 0 {
        return Ok(json!({"oldestBlock": q(0), "gasUsedRatio": null}));
    }
    if pct.len() > FEE_HISTORY_MAX_PERCENTILES {
        return Err(RpcError::server(format!(
            "invalid reward percentile: over the query limit {FEE_HISTORY_MAX_PERCENTILES}"
        )));
    }
    for (i, v) in pct.iter().enumerate() {
        if !(0.0..=100.0).contains(v) {
            return Err(RpcError::server(format!(
                "invalid reward percentile: {v:.6}"
            )));
        }
        if let Some(prev) = i.checked_sub(1).and_then(|j| pct.get(j)) {
            if v <= prev {
                return Err(RpcError::server(format!(
                    "invalid reward percentile: #{}:{prev:.6} >= #{i}:{v:.6}",
                    i - 1
                )));
            }
        }
    }
    let head = c.be.chain.head().0;
    let last = p.number_or_latest(1)?;
    if last > head {
        return Err(RpcError::server(format!(
            "request beyond head block: requested {last}, head {head}"
        )));
    }
    let count = count.min(FEE_HISTORY_MAX_BLOCKS).min(last + 1);
    let first = last + 1 - count;
    let spec = c.be.chain.spec();
    let (mut fees, mut ratio, mut rewards) = (vec![], vec![], vec![]);
    let mut next_fee = q(0);
    for n in first..=last {
        let b = block_of(c, BlockRef::Number(n))?.ok_or_else(no_header)?;
        let base = b.header.base_fee;
        fees.push(qu(base.unwrap_or(U256::ZERO)));
        // geth: `CalcBaseFee` of the child when London is active there; on XDC the tier of n + 1
        next_fee = if spec.london.is_active(n + 1) {
            q(xone_evm::basefee::xdc_base_fee_for_block(spec, n + 1))
        } else {
            q(0)
        };
        ratio.push(json!(if b.header.gas_limit == 0 {
            0.0
        } else {
            f64_of(b.header.gas_used) / f64_of(b.header.gas_limit)
        }));
        if pct.is_empty() {
            continue;
        }
        let mut txs: Vec<(U256, u64)> = Vec::with_capacity(b.txs.len());
        if !b.txs.is_empty() {
            // the percentiles need every receipt: an unavailable one is an error, not a guess
            c.be.chain.history(Class::Receipt, n)?;
            let raw =
                c.be.chain
                    .receipts(b.hash)
                    .ok_or_else(|| RpcError::server(format!("receipts of block {n} not found")))?;
            let rc = decode_receipts(&raw)?;
            if rc.len() != b.txs.len() {
                return Err(RpcError::server(format!(
                    "receipts of block {n} do not match its transactions"
                )));
            }
            let mut prev = 0u64;
            for (t, r) in b.txs.iter().zip(&rc) {
                let tx = xone_executor::tx::decode(t, spec.chain_id)
                    .map_err(|e| RpcError::internal(e.to_string()))?;
                let tip_cap = tx.priority_fee.unwrap_or(tx.gas_price);
                let used = r.cumulative_gas_used.saturating_sub(prev);
                prev = r.cumulative_gas_used;
                txs.push((effective_tip(tx.gas_price, tip_cap, base), used));
            }
        }
        rewards.push(Value::Array(reward_row(b.header.gas_used, txs, &pct)));
    }
    fees.push(next_fee);
    // No blob transactions on XDC (geth skips blob fees `!config.IsXDC()`): zeros, the fee
    // series one longer.
    let zeros = vec![q(0); fees.len()];
    let mut m = json!({"oldestBlock": q(first), "baseFeePerGas": fees, "gasUsedRatio": ratio, "baseFeePerBlobGas": zeros, "blobGasUsedRatio": vec![0; ratio.len()]});
    if !pct.is_empty() {
        m["reward"] = Value::Array(rewards);
    }
    Ok(m)
}

#[cfg(test)]
mod fee_history_tests {
    use super::*;

    #[test]
    fn effective_tip_is_geths() {
        let g = |x: u64| U256::from(x);
        // no base fee: the tip cap
        assert_eq!(effective_tip(100, 7, None), g(7));
        // under the base fee: geth leaves the fee cap
        assert_eq!(effective_tip(10, 7, Some(g(12))), g(10));
        assert_eq!(effective_tip(100, 7, Some(g(12))), g(7));
        assert_eq!(effective_tip(15, 7, Some(g(12))), g(3));
    }

    #[test]
    fn reward_rows_use_full_gas_and_fractional_percentiles() {
        let g = |x: u64| U256::from(x);
        // gas values above u32: the threshold uses the whole number
        let big = 6_000_000_000u64;
        let txs = vec![(g(30), big), (g(10), big), (g(20), big)];
        let row = reward_row(3 * big, txs, &[0.0, 33.3, 33.4, 66.7, 100.0]);
        let want = [q(10), q(10), q(20), q(30), q(30)];
        assert_eq!(row, want);
        // an empty block: zeros
        assert_eq!(reward_row(0, vec![], &[10.0, 90.5]), [q(0), q(0)]);
        // geth's walk stops at the first tx whose running sum reaches the threshold
        let row = reward_row(100, vec![(g(1), 50), (g(2), 50)], &[49.9, 50.0, 50.9, 51.0]);
        // 50.9% of 100 truncates to 50, as geth's uint64(float64)
        assert_eq!(row, [q(1), q(1), q(1), q(2)]);
    }
}
