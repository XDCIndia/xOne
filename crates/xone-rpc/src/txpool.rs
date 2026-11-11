//! `txpool_*` (geth `internal/ethapi/api.go` `TxPoolAPI`: `Content`, `Status`, `Inspect`).

use crate::{
    json::{q, tx_json},
    server::{Cost, Ctx, Registry},
    types::{RpcError, R},
};
use alloy_primitives::{Address, Bytes};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub fn register(r: &mut Registry) {
    r.add("txpool_status", Cost::C0, |c, _| {
        let (p, qd) = c.be.pool.status();
        Ok(json!({"pending": q(u64::try_from(p).unwrap_or(u64::MAX)), "queued": q(u64::try_from(qd).unwrap_or(u64::MAX))}))
    });
    r.add("txpool_content", Cost::C2, |c, _| {
        render(c, |raw, id| tx_json(raw, id, None))
    });
    r.add("txpool_inspect", Cost::C2, |c, _| render(c, inspect_line));
}

/// `geth` `fmt.Sprintf("%s: %v wei + %v gas x %v wei", to, value, gas, gasPrice)`.
fn inspect_line(raw: &Bytes, chain_id: u64) -> R {
    let t =
        xone_executor::tx::decode(raw, chain_id).map_err(|e| RpcError::internal(e.to_string()))?;
    let to =
        t.to.map_or_else(|| "contract creation".to_owned(), |a| format!("{a:#x}"));
    Ok(Value::String(format!(
        "{to}: {} wei + {} gas \u{d7} {} wei",
        t.value, t.gas, t.gas_price
    )))
}

/// `{"pending": {from: {nonce: x}}, "queued": ...}`, nonces as decimal strings.
fn render(c: &Ctx, f: fn(&Bytes, u64) -> R) -> R {
    let id = c.be.chain.spec().chain_id;
    let (pending, queued) = c.be.pool.content();
    let side = |m: BTreeMap<Address, Vec<Bytes>>| -> R {
        let mut o = Map::new();
        for (from, txs) in m {
            let mut by = Map::new();
            for raw in txs {
                let nonce = xone_executor::tx::decode(&raw, id)
                    .map_err(|e| RpcError::internal(e.to_string()))?
                    .nonce;
                by.insert(nonce.to_string(), f(&raw, id)?);
            }
            o.insert(format!("{from:#x}"), Value::Object(by));
        }
        Ok(Value::Object(o))
    };
    Ok(json!({"pending": side(pending)?, "queued": side(queued)?}))
}
