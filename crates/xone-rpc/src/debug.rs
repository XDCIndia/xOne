//! `debug_*` subset (geth `eth/api_debug.go`, `eth/tracers/api.go`): `getRawHeader`, `getRawBlock`
//! and `traceTransaction`, `traceBlockByNumber`, `traceBlockByHash` with the native tracers
//! `callTracer`, `prestateTracer`, `4byteTracer`, `noopTracer` and `muxTracer`
//! (`tracers/native/call.go`, `prestate.go`, `4byte.go`, `noop.go`, `mux.go`). Class C3.

use crate::{
    api::{CallFrame, PreAccount, TraceOut, Tracer},
    eth::{block_of, header_of},
    json::{addr, h256, header_rlp, hex, q, qu},
    server::{Cost, Ctx, Registry},
    types::{BlockRef, Params, RpcError, R},
};
use alloy_primitives::{keccak256, Address, B256};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
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

/// A `tracer` choice. The native ones run in the executor; `noopTracer` and `4byteTracer` are
/// derived from a `callTracer` run (their geth hooks see the same `OnEnter` frames), and
/// `muxTracer` runs each of its tracers and keys the results by name (geth `native/mux.go`).
#[derive(Debug, Clone, PartialEq)]
enum Choice {
    Native(Tracer),
    Noop,
    FourByte,
    Mux(Vec<(String, Choice)>),
}

fn tracer(p: &Params, i: usize) -> Result<Choice, RpcError> {
    let cfg = p.opt(i).and_then(Value::as_object);
    let name = cfg.and_then(|o| o.get("tracer")).and_then(Value::as_str);
    let Some(name) = name else {
        // The struct logger (no tracer) is not part of ADR-008.
        return Err(RpcError::server(
            "tracer not supported: set \"tracer\" to callTracer, prestateTracer, 4byteTracer, noopTracer or muxTracer",
        ));
    };
    choice(name, cfg.and_then(|o| o.get("tracerConfig")))
}

fn choice(name: &str, tc: Option<&Value>) -> Result<Choice, RpcError> {
    let tc = tc.and_then(Value::as_object);
    let flag = |k: &str| {
        tc.and_then(|o| o.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    match name {
        "callTracer" => Ok(Choice::Native(Tracer::Call {
            only_top_call: flag("onlyTopCall"),
            with_log: flag("withLog"),
        })),
        "prestateTracer" => Ok(Choice::Native(Tracer::Prestate {
            diff_mode: flag("diffMode"),
        })),
        "noopTracer" => Ok(Choice::Noop),
        "4byteTracer" => Ok(Choice::FourByte),
        "muxTracer" => {
            let subs = tc
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| Ok((k.clone(), choice(k, Some(v))?)))
                        .collect::<Result<Vec<_>, RpcError>>()
                })
                .transpose()?
                .unwrap_or_else(Vec::new);
            Ok(Choice::Mux(subs))
        }
        // The JS tracers are not part of ADR-008.
        other => Err(RpcError::server(format!("tracer not found: {other}"))),
    }
}

/// geth `fourByteTracer.OnEnter`: count `selector-(len(input)-4)` for each CALL, CALLCODE,
/// DELEGATECALL and STATICCALL frame (the top call included) with at least 4 bytes of input,
/// skipping calls to the `ActivePrecompiles` set of the block.
fn four_byte(f: &CallFrame, precompiles: &BTreeSet<Address>, ids: &mut BTreeMap<String, u64>) {
    let is_call = matches!(
        f.kind.as_str(),
        "CALL" | "CALLCODE" | "DELEGATECALL" | "STATICCALL"
    );
    let to_precompile = f.to.is_some_and(|t| precompiles.contains(&t));
    if let (true, false, Some(sel)) = (is_call, to_precompile, f.input.get(..4)) {
        let size = f.input.len().saturating_sub(4);
        let key = format!("0x{}-{size}", alloy_primitives::hex::encode(sel));
        *ids.entry(key).or_insert(0) += 1;
    }
    for c in &f.calls {
        four_byte(c, precompiles, ids);
    }
}

/// geth `vm.ActivePrecompiles` at `number` (the access-list set, without the XDCx ones).
fn active_precompiles(c: &Ctx, number: u64) -> Result<BTreeSet<Address>, RpcError> {
    let spec = c.be.chain.spec();
    let rules = xone_evm::spec::rules(spec, number)
        .map_err(|e| RpcError::server(format!("fork rules: {e}")))?;
    Ok(xone_evm::precompiles::row(spec, &rules, number).warm())
}

/// Runs `t` on transaction `index` (or every transaction) of `block`; one result per transaction.
fn run(
    c: &Ctx,
    block: B256,
    number: u64,
    index: Option<usize>,
    t: &Choice,
) -> Result<Vec<Value>, RpcError> {
    let call_frames = |only_top_call| {
        c.be.state.trace(
            block,
            index,
            &Tracer::Call {
                only_top_call,
                with_log: false,
            },
            c.cancel,
        )
    };
    match t {
        Choice::Native(n) => Ok(c
            .be
            .state
            .trace(block, index, n, c.cancel)?
            .iter()
            .map(trace_json)
            .collect()),
        Choice::Noop => Ok(call_frames(true)?
            .iter()
            .map(|_| Value::Object(Map::new()))
            .collect()),
        Choice::FourByte => {
            let pre = active_precompiles(c, number)?;
            Ok(call_frames(false)?
                .iter()
                .map(|o| {
                    let mut ids = BTreeMap::new();
                    if let TraceOut::Call(f) = o {
                        four_byte(f, &pre, &mut ids);
                    }
                    Value::Object(ids.into_iter().map(|(k, n)| (k, json!(n))).collect())
                })
                .collect())
        }
        Choice::Mux(subs) => {
            let mut per_tx: Vec<Map<String, Value>> = Vec::new();
            for (name, sub) in subs {
                let out = run(c, block, number, index, sub)?;
                if per_tx.len() < out.len() {
                    per_tx.resize_with(out.len(), Map::new);
                }
                for (m, v) in per_tx.iter_mut().zip(out) {
                    m.insert(name.clone(), v);
                }
            }
            if subs.is_empty() {
                // geth runs no hooks and returns `{}` for each transaction.
                return Ok(call_frames(true)?
                    .iter()
                    .map(|_| Value::Object(Map::new()))
                    .collect());
            }
            Ok(per_tx.into_iter().map(Value::Object).collect())
        }
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
    let mut out = run(
        c,
        bh,
        b.header.number,
        Some(usize::try_from(i).unwrap_or(usize::MAX)),
        &t,
    )?;
    out.pop()
        .ok_or_else(|| RpcError::server("transaction not found"))
}

fn trace_block(c: &Ctx, p: &Params, r: BlockRef) -> R {
    let t = tracer(p, 1)?;
    let Some(b) = block_of(c, r)? else {
        return Err(RpcError::server("block not found"));
    };
    c.be.chain
        .history(Class::State, b.header.number.saturating_sub(1))?;
    let out = run(c, b.hash, b.header.number, None, &t)?;
    Ok(Value::Array(
        out.into_iter()
            .zip(&b.txs)
            .map(|(o, raw)| json!({"txHash": h256(keccak256(raw)), "result": o}))
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;

    fn call(kind: &str, to: u64, input: &[u8], calls: Vec<CallFrame>) -> CallFrame {
        CallFrame {
            kind: kind.into(),
            to: Some(Address::with_last_byte(u8::try_from(to).unwrap_or(0xff))),
            input: Bytes::copy_from_slice(input),
            calls,
            ..CallFrame::default()
        }
    }

    #[test]
    fn four_byte_counts_calls_and_skips_creates_precompiles_and_short_input() {
        let sel = [0xa9, 0x05, 0x9c, 0xbb];
        let mut long = sel.to_vec();
        long.extend([0u8; 64]);
        let top = call(
            "CALL",
            0x50,
            &long,
            vec![
                call("STATICCALL", 0x51, &sel, vec![]),
                call("DELEGATECALL", 0x52, &long, vec![]),
                // a precompile, a create and short input are not counted
                call("STATICCALL", 0x02, &long, vec![]),
                call("CREATE", 0x53, &long, vec![]),
                call("CALL", 0x54, &[1, 2, 3], vec![]),
                call("CALLCODE", 0x55, &sel, vec![]),
            ],
        );
        let pre: BTreeSet<Address> = [Address::with_last_byte(2)].into();
        let mut ids = BTreeMap::new();
        four_byte(&top, &pre, &mut ids);
        let want: BTreeMap<String, u64> = [
            ("0xa9059cbb-64".to_owned(), 2),
            ("0xa9059cbb-0".to_owned(), 2),
        ]
        .into();
        assert_eq!(ids, want);
    }

    fn parse(cfg: Value) -> Result<Choice, RpcError> {
        let v = [json!("0x00"), cfg];
        tracer(
            &Params {
                v: &v,
                head: 0,
                finalized: None,
            },
            1,
        )
    }

    #[test]
    fn tracer_names() {
        assert_eq!(
            parse(json!({"tracer": "noopTracer"})).ok(),
            Some(Choice::Noop)
        );
        assert_eq!(
            parse(json!({"tracer": "4byteTracer"})).ok(),
            Some(Choice::FourByte)
        );
        let mux = parse(json!({"tracer": "muxTracer", "tracerConfig": {
            "callTracer": {"onlyTopCall": true},
            "4byteTracer": {},
        }}));
        assert_eq!(
            mux.ok(),
            Some(Choice::Mux(vec![
                ("4byteTracer".into(), Choice::FourByte),
                (
                    "callTracer".into(),
                    Choice::Native(Tracer::Call {
                        only_top_call: true,
                        with_log: false
                    })
                ),
            ]))
        );
        assert!(parse(json!({"tracer": "muxTracer", "tracerConfig": {"jsTracer": {}}})).is_err());
        assert!(parse(json!({"tracer": "flatCallTracer"})).is_err());
        assert!(parse(json!({})).is_err());
    }
}
