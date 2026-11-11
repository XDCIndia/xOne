//! The `XDPoS` namespace, PascalCase and case-sensitive (geth `consensus/XDPoS/api.go:41-184`,
//! `api_extended.go:119-395`). The set is exactly [`METHODS`]; `xdpos_getSigners` is `-32601`.

use crate::{
    json::{addr, h256},
    server::{Cost, Ctx, Registry},
    types::{Params, RpcError, R},
};
use alloy_primitives::{Address, B256};
use serde_json::{json, Map, Value};
use xone_primitives::Header;

/// The namespace's method set, as served by xOneGo `pkg/rpc/xdposapi` (13) plus `getV2BlockByNumber`.
pub const METHODS: [&str; 14] = [
    "XDPoS_getBlockInfoByEpochNum",
    "XDPoS_getCandidates",
    "XDPoS_getEpochNumbersBetween",
    "XDPoS_getLatestPoolStatus",
    "XDPoS_getMasternodes",
    "XDPoS_getMasternodesByNumber",
    "XDPoS_getMissedRoundsInEpochByBlockNum",
    "XDPoS_getRewardByAccount",
    "XDPoS_getSigners",
    "XDPoS_getSignersAtHash",
    "XDPoS_getSnapshot",
    "XDPoS_getSnapshotAtHash",
    "XDPoS_getV2BlockByNumber",
    "XDPoS_networkInformation",
];

pub fn register(r: &mut Registry) {
    r.add("XDPoS_getSnapshot", Cost::C1, |c, p| snapshot(c, p, |s| s));
    r.add("XDPoS_getSnapshotAtHash", Cost::C1, |c, p| {
        snapshot_at(c, p, |s| s)
    });
    r.add("XDPoS_getSigners", Cost::C1, |c, p| snapshot(c, p, signers));
    r.add("XDPoS_getSignersAtHash", Cost::C1, |c, p| {
        snapshot_at(c, p, signers)
    });
    // geth `API.GetCandidates` returns the snapshot's signers (`api.go:184-199`).
    r.add("XDPoS_getCandidates", Cost::C1, |c, p| {
        snapshot(c, p, signers)
    });
    r.add("XDPoS_getMasternodes", Cost::C1, masternodes);
    r.add(
        "XDPoS_getMasternodesByNumber",
        Cost::C1,
        masternodes_by_number,
    );
    r.add("XDPoS_getV2BlockByNumber", Cost::C1, v2_block);
    r.add("XDPoS_getBlockInfoByEpochNum", Cost::C1, epoch_info);
    r.add("XDPoS_getEpochNumbersBetween", Cost::C1, epochs_between);
    r.add(
        "XDPoS_getMissedRoundsInEpochByBlockNum",
        Cost::C1,
        missed_rounds,
    );
    r.add("XDPoS_getLatestPoolStatus", Cost::C0, |_, _| {
        Ok(json!({"votePool": 0, "timeoutPool": 0}))
    });
    r.add("XDPoS_networkInformation", Cost::C0, network_information);
    r.add("XDPoS_getRewardByAccount", Cost::C1, |c, p| {
        let (b, e) = (p.number_or_latest(1)?, p.number_or_latest(2)?);
        c.be.consensus.reward_by_account(p.address(0)?, b, e)
    });
}

fn unknown_block() -> RpcError {
    RpcError::server("unknown block")
}

/// `getHeaderFromApiBlockNum`: nil and `latest` are the head, a number above it is `None`.
fn header_at(c: &Ctx, p: &Params, i: usize) -> Result<Option<(Header, B256)>, RpcError> {
    let n = p.number_or_latest(i)?;
    let Some(h) = c.be.chain.hash_by_number(n) else {
        return Ok(None);
    };
    Ok(c.be.chain.header(h).map(|x| (x, h)))
}

/// `votes` is Go's nil slice (`null`) for a snapshot made by `newSnapshot` (genesis and checkpoints)
/// and `[]` for one made by `apply` (`snapshot.go:59-73`, `:121-129`; archive vectors).
fn snapshot_json(epoch: u64, s: &xone_consensus::v1::snapshot::Snapshot) -> Value {
    let signers: Map<String, Value> = s
        .signers
        .iter()
        .map(|a| (format!("{a:#x}"), json!({})))
        .collect();
    let recents: Map<String, Value> = s
        .recents
        .iter()
        .map(|(n, a)| (n.to_string(), addr(*a)))
        .collect();
    let votes = if s.votes.is_empty() {
        if s.number.is_multiple_of(epoch.max(1)) {
            Value::Null
        } else {
            json!([])
        }
    } else {
        Value::Array(s.votes.iter().map(|v| json!({"signer": addr(v.signer), "block": v.block, "address": addr(v.address), "authorize": v.authorize})).collect())
    };
    let tally: Map<String, Value> = s
        .tally
        .iter()
        .map(|(a, t)| {
            (
                format!("{a:#x}"),
                json!({"authorize": t.authorize, "votes": t.votes}),
            )
        })
        .collect();
    json!({"number": s.number, "hash": h256(s.hash), "signers": signers, "recents": recents, "votes": votes, "tally": tally})
}

fn signers(s: Value) -> Value {
    // The snapshot object's signer keys are the list, ascending (`Snapshot.GetSigners`).
    Value::Array(s["signers"].as_object().map_or_else(Vec::new, |m| {
        m.keys().map(|k| Value::String(k.clone())).collect()
    }))
}

fn snapshot(c: &Ctx, p: &Params, f: fn(Value) -> Value) -> R {
    let (_, h) = header_at(c, p, 0)?.ok_or_else(unknown_block)?;
    Ok(f(snapshot_json(
        c.be.chain.spec().xdpos.epoch,
        &c.be.consensus.snapshot(h)?,
    )))
}

fn snapshot_at(c: &Ctx, p: &Params, f: fn(Value) -> Value) -> R {
    let h = p.hash(0)?;
    c.be.chain.header(h).ok_or_else(unknown_block)?;
    Ok(f(snapshot_json(
        c.be.chain.spec().xdpos.epoch,
        &c.be.consensus.snapshot(h)?,
    )))
}

fn addrs(v: &[Address]) -> Value {
    Value::Array(v.iter().map(|a| addr(*a)).collect())
}

fn masternodes(c: &Ctx, p: &Params) -> R {
    let (_, h) = header_at(c, p, 0)?.ok_or_else(unknown_block)?;
    Ok(addrs(&c.be.consensus.masternodes(h)?))
}

fn is_v2(c: &Ctx, number: u64) -> bool {
    c.be.chain
        .spec()
        .xdpos
        .v2
        .as_ref()
        .is_some_and(|v| number > v.switch_block)
}

fn v2_block(c: &Ctx, p: &Params) -> R {
    let Some((h, hash)) = header_at(c, p, 0)? else {
        return Ok(
            json!({"hash": h256(B256::ZERO), "number": null, "round": 0, "masternodes": null, "encodedQC": "", "error": "block not found"}),
        );
    };
    let mut m = json!({"hash": h256(hash), "number": h.number, "round": 0, "masternodes": null, "encodedQC": ""});
    if is_v2(c, h.number) {
        if let Ok(r) = c.be.consensus.round(hash) {
            m["round"] = json!(r);
        }
        m["masternodes"] = addrs(&c.be.consensus.masternodes(hash)?);
        // F-2026-19639: the QC the header carries, re-encoded by RLP and base64.
        if let Ok(x) = h.extra_fields_v2() {
            let mut enc = Vec::new();
            alloy_rlp::Encodable::encode(&x.quorum_cert, &mut enc);
            m["encodedQC"] = json!(crate::json::base64(&enc));
        }
    }
    Ok(m)
}

/// `masternodesStatus` (`api_extended.go:171-248`). Fields are PascalCase; the tier keys are omitted
/// when empty; `Error` is `null` on success. The mainnet shape (no `Epoch`) is `chain_id == 50`.
fn masternodes_by_number(c: &Ctx, p: &Params) -> R {
    let Some((h, hash)) = header_at(c, p, 0)? else {
        let n = p.number_or_latest(0)?;
        return Ok(json!({"Error": format!("can not get header by number {n}")}));
    };
    let spec = c.be.chain.spec();
    let (mut round, mut mn, mut pen, mut standby) = (0u64, vec![], vec![], vec![]);
    if is_v2(c, h.number) {
        round = c.be.consensus.round(hash)?;
        mn = c.be.consensus.masternodes(hash)?;
        pen = c.be.consensus.penalties(hash)?;
        standby = c.be.consensus.standbynodes(hash)?;
    }
    let v2 = spec.xdpos.v2.as_ref();
    let epoch = v2.map_or(0, |v| v.switch_epoch) + round / spec.xdpos.epoch.max(1);
    let mut m = Map::new();
    let mainnet = spec.chain_id == 50;
    if !mainnet {
        m.insert("Epoch".into(), json!(epoch));
    }
    m.insert("Number".into(), json!(h.number));
    m.insert("Round".into(), json!(round));
    m.insert("MasternodesLen".into(), json!(mn.len()));
    m.insert("Masternodes".into(), addrs(&mn));
    m.insert("PenaltyLen".into(), json!(pen.len()));
    m.insert("Penalty".into(), addrs(&pen));
    let (max_p, max_o) = v2
        .and_then(|v| v.config_at_round(round))
        .map_or((0, 0), |cfg| {
            (
                usize::try_from(cfg.max_protector_nodes).unwrap_or(0),
                usize::try_from(cfg.max_observer_nodes).unwrap_or(0),
            )
        });
    let tiered = matches!(
        xone_consensus::reward::rule(spec, h.number),
        xone_consensus::reward::Rule::Tiered { .. }
    );
    let (prot, obs, stand) = if tiered {
        let pe = max_p.min(standby.len());
        let oe = (pe + max_o).min(standby.len());
        (
            standby[..pe].to_vec(),
            standby[pe..oe].to_vec(),
            standby[oe..].to_vec(),
        )
    } else {
        (vec![], vec![], standby)
    };
    if !prot.is_empty() {
        m.insert("ProtectorLen".into(), json!(prot.len()));
        m.insert("Protectornodes".into(), addrs(&prot));
    }
    if !obs.is_empty() {
        m.insert("ObserverLen".into(), json!(obs.len()));
        m.insert("Observernodes".into(), addrs(&obs));
    }
    if !stand.is_empty() || mainnet {
        m.insert("StandbynodesLen".into(), json!(stand.len()));
        m.insert("Standbynodes".into(), addrs(&stand));
    }
    m.insert("Error".into(), Value::Null);
    Ok(Value::Object(m))
}

fn epoch_info(c: &Ctx, p: &Params) -> R {
    let epoch = p
        .opt(0)
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::invalid_params("invalid argument 0: expected uint64"))?;
    let spec = c.be.chain.spec();
    if let Some(v2) = spec.xdpos.v2.as_ref().filter(|v| epoch >= v.switch_epoch) {
        let _ = v2;
        let i = c.be.consensus.epoch_info(epoch)?;
        return Ok(
            json!({"hash": h256(i.hash), "round": i.round, "firstBlock": i.first_block, "lastBlock": i.last_block}),
        );
    }
    // V1: the epoch switch block is `epoch * Epoch`.
    let e = spec.xdpos.epoch;
    let first = epoch.saturating_mul(e);
    Ok(
        json!({"hash": h256(B256::ZERO), "round": first, "firstBlock": first, "lastBlock": first + e - 1}),
    )
}

fn epochs_between(c: &Ctx, p: &Params) -> R {
    let (Some((b, bh)), Some((e, eh))) = (header_at(c, p, 0)?, header_at(c, p, 1)?) else {
        return Ok(Value::Null);
    };
    if is_v2(c, b.number) {
        // `maxEpochSpan` 1000 epochs (`api_extended.go:259-266`).
        let epoch = c.be.chain.spec().xdpos.epoch;
        if e.number > b.number && e.number - b.number > 1000 * epoch {
            return Err(RpcError::server(format!(
                "range spans {} blocks, exceeding the 1000-epoch limit of {} blocks",
                e.number - b.number,
                1000 * epoch
            )));
        }
        let v = c.be.consensus.epoch_switches_between(bh, eh)?;
        // CertiK XDC-30: `begin == end` is a valid, possibly empty, range.
        return Ok(Value::Array(v.into_iter().map(Value::from).collect()));
    }
    Err(RpcError::server("not supported in the v1 consensus"))
}

fn missed_rounds(c: &Ctx, p: &Params) -> R {
    let Some((h, hash)) = header_at(c, p, 0)? else {
        return Ok(Value::Null);
    };
    if is_v2(c, h.number) {
        let m = c.be.consensus.missed_rounds(hash)?;
        let rows: Value = if m.missed.is_empty() {
            Value::Null
        } else {
            Value::Array(m.missed.iter().map(|r| json!({"Round": r.round, "Miner": addr(r.miner), "CurrentBlockHash": h256(r.current_hash), "CurrentBlockNum": r.current_number, "ParentBlockHash": h256(r.parent_hash), "ParentBlockNum": r.parent_number})).collect())
        };
        return Ok(
            json!({"EpochRound": m.epoch_round, "EpochBlockNumber": m.epoch_block_number, "MissedRounds": rows}),
        );
    }
    let e = c.be.chain.spec().xdpos.epoch.max(1);
    let start = h.number - h.number % e;
    Ok(json!({"EpochRound": start, "EpochBlockNumber": start, "MissedRounds": null}))
}

fn network_information(c: &Ctx, _: &Params) -> R {
    let spec = c.be.chain.spec();
    let v2 = spec.xdpos.v2.as_ref();
    let validators = v2
        .and_then(|v| v.configs.values().next_back())
        .map_or(18, |cfg| cfg.max_masternodes);
    let mut m =
        json!({"networkId": spec.chain_id, "xdcValidators": validators, "epoch": spec.xdpos.epoch});
    if let Some(v) = v2 {
        m["v2SwitchBlock"] = json!(v.switch_block);
    }
    Ok(m)
}
