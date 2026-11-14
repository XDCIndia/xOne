//! `debug_*` subset (geth `eth/api_debug.go`, `eth/tracers/api.go`): `getRawHeader`, `getRawBlock`
//! and `traceTransaction`, `traceBlockByNumber`, `traceBlockByHash` with the two native tracers
//! `callTracer` and `prestateTracer` (`tracers/native/call.go`, `prestate.go`). Class C3.

use crate::{
    api::{CallFrame, PreAccount, TraceOut, Tracer},
    eth::{block_of, header_of},
    json::{addr, h256, header_rlp, hex, q, qu},
    server::{Cost, Ctx, Registry},
    types::{BlockRef, Params, RpcError, R},
};
use alloy_primitives::{keccak256, Address};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use xone_sync::bootstrap::retention::Class;

pub fn register(r: &mut Registry) {
    r.add("debug_getRawHeader", Cost::C1, |c, p| {
        Ok(header_of(c, p.block_ref(0)?)?.map_or(Value::Null, |h| hex(&header_rlp(&h))))
    });
    r.add("debug_getRawBlock", Cost::C1, |c, p| {
        Ok(block_of(c, p.block_ref(0)?)?.map_or(Value::Null, |b| hex(&b.raw)))
    });
    r.add("debug_traceTransaction", Cost::C3, trace_tx);
    r.add("debug_traceBlockByNumber", Cost::C3, |c, p| {
        trace_block(c, p, BlockRef::Number(p.number_or_latest(0)?))
    });
    r.add("debug_traceBlockByHash", Cost::C3, |c, p| {
        trace_block(c, p, BlockRef::Hash(p.hash(0)?))
    });
}

fn tracer(p: &Params, i: usize) -> Result<Tracer, RpcError> {
    let cfg = p.opt(i).and_then(Value::as_object);
    let name = cfg.and_then(|o| o.get("tracer")).and_then(Value::as_str);
    let tc = cfg
        .and_then(|o| o.get("tracerConfig"))
        .and_then(Value::as_object);
    let flag = |k: &str| {
        tc.and_then(|o| o.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    match name {
        Some("callTracer") => Ok(Tracer::Call {
            only_top_call: flag("onlyTopCall"),
            with_log: flag("withLog"),
        }),
        Some("prestateTracer") => Ok(Tracer::Prestate {
            diff_mode: flag("diffMode"),
        }),
        // The struct logger (no tracer) and the JS tracers are not part of ADR-008.
        Some(other) => Err(RpcError::server(format!("tracer not found: {other}"))),
        None => Err(RpcError::server(
            "tracer not supported: set \"tracer\" to callTracer or prestateTracer",
        )),
    }
}

fn trace_tx(c: &Ctx, p: &Params) -> R {
    let t = tracer(p, 1)?;
    let h = p.hash(0)?;
    let Some((bh, i)) = c.be.chain.tx_location(h) else {
        return Err(RpcError::server("transaction not found"));
    };
    let b =
        c.be.chain
            .block(bh)
            .ok_or_else(|| RpcError::server("block not found"))?;
    // The parent state: a trace re-executes on it.
    c.be.chain
        .history(Class::State, b.header.number.saturating_sub(1))?;
    let mut out = c.be.state.trace(
        bh,
        Some(usize::try_from(i).unwrap_or(usize::MAX)),
        &t,
        c.cancel,
    )?;
    out.pop()
        .map(|o| trace_json(&o))
        .ok_or_else(|| RpcError::server("transaction not found"))
}

fn trace_block(c: &Ctx, p: &Params, r: BlockRef) -> R {
    let t = tracer(p, 1)?;
    let Some(b) = block_of(c, r)? else {
        return Err(RpcError::server("block not found"));
    };
    c.be.chain
        .history(Class::State, b.header.number.saturating_sub(1))?;
    let out = c.be.state.trace(b.hash, None, &t, c.cancel)?;
    Ok(Value::Array(
        out.iter()
            .zip(&b.txs)
            .map(|(o, raw)| json!({"txHash": h256(keccak256(raw)), "result": trace_json(o)}))
            .collect(),
    ))
}

/// geth `callFrame` marshaling: `type`, `from`, `gas`, `gasUsed`, `to`, `input`, `output`, `error`,
/// `revertReason`, `calls`, `value`; the optional ones are omitted when empty.
fn frame(f: &CallFrame) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), json!(f.kind));
    m.insert("from".into(), addr(f.from));
    m.insert("gas".into(), q(f.gas));
    m.insert("gasUsed".into(), q(f.gas_used));
    if let Some(t) = f.to {
        m.insert("to".into(), addr(t));
    }
    m.insert("input".into(), hex(&f.input));
    if let Some(o) = f.output.as_ref().filter(|o| !o.is_empty()) {
        m.insert("output".into(), hex(o));
    }
    if let Some(e) = &f.error {
        m.insert("error".into(), json!(e));
    }
    if let Some(e) = &f.revert_reason {
        m.insert("revertReason".into(), json!(e));
    }
    if !f.calls.is_empty() {
        m.insert(
            "calls".into(),
            Value::Array(f.calls.iter().map(frame).collect()),
        );
    }
    if let Some(v) = f.value {
        m.insert("value".into(), qu(v));
    }
    Value::Object(m)
}

fn accounts(m: &BTreeMap<Address, PreAccount>) -> Value {
    Value::Object(
        m.iter()
            .map(|(a, x)| {
                let mut o = Map::new();
                if let Some(b) = x.balance {
                    o.insert("balance".into(), qu(b));
                }
                if let Some(n) = x.nonce.filter(|n| *n != 0) {
                    o.insert("nonce".into(), json!(n));
                }
                if let Some(c) = x.code.as_ref().filter(|c| !c.is_empty()) {
                    o.insert("code".into(), hex(c));
                }
                if !x.storage.is_empty() {
                    o.insert(
                        "storage".into(),
                        Value::Object(
                            x.storage
                                .iter()
                                .map(|(k, v)| (format!("{k:#x}"), h256(*v)))
                                .collect(),
                        ),
                    );
                }
                (format!("{a:#x}"), Value::Object(o))
            })
            .collect(),
    )
}

/// The result of one tracer, as geth returns it for `traceTransaction`.
pub fn trace_json(o: &TraceOut) -> Value {
    match o {
        TraceOut::Call(f) => frame(f),
        TraceOut::Prestate { pre, post: None } => accounts(pre),
        TraceOut::Prestate {
            pre,
            post: Some(post),
        } => json!({"post": accounts(post), "pre": accounts(pre)}),
    }
}
