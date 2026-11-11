//! The XDC `eth_*` extensions (geth `internal/ethapi/api_xdc.go:58-300`): block signers, finality,
//! candidate status and candidates. Each reads through the consensus engine or the 0x88 contract.

use crate::{
    json::{addr, bigint},
    server::{Cost, Ctx, Registry},
    types::{BlockRef, Params, RpcError, R},
};
use alloy_primitives::{Address, B256, U256};
use serde_json::{json, Value};
use xone_primitives::Header;
use xone_state::{system_contracts::validator_0x88 as vc, StateRead};
use xone_sync::bootstrap::retention::Class;

pub fn register(r: &mut Registry) {
    r.add("eth_getBlockSignersByHash", Cost::C1, |c, p| {
        signers(c, BlockRef::Hash(p.hash(0)?))
    });
    r.add("eth_getBlockSignersByNumber", Cost::C1, |c, p| {
        signers(c, BlockRef::Number(p.number_or_latest(0)?))
    });
    r.add("eth_getBlockFinalityByHash", Cost::C1, |c, p| {
        finality(c, BlockRef::Hash(p.hash(0)?))
    });
    r.add("eth_getBlockFinalityByNumber", Cost::C1, |c, p| {
        finality(c, BlockRef::Number(p.number_or_latest(0)?))
    });
    // `GetMasternodes(ctx, *types.Block)` is exported by accident: over RPC the argument is a block
    // object and every client string fails to decode (`api_xdc.go:117`; archive vectors).
    r.add("eth_getMasternodes", Cost::C1, |_, _| {
        Err(RpcError::invalid_params(
            "invalid argument 0: json: cannot unmarshal string into Go value of type types.Block",
        ))
    });
    r.add("eth_getCandidateStatus", Cost::C1, candidate_status);
    r.add("eth_getCandidates", Cost::C1, candidates);
}

/// `&dyn StateRead` as a sized `StateRead`, for the 0x88 readers.
struct Dyn<'a>(&'a dyn StateRead);
impl StateRead for Dyn<'_> {
    fn account(&self, a: Address) -> Option<xone_state::Account> {
        self.0.account(a)
    }
    fn storage(&self, a: Address, k: B256) -> U256 {
        self.0.storage(a, k)
    }
}

fn resolve(c: &Ctx, r: BlockRef) -> Result<Option<B256>, RpcError> {
    Ok(match r {
        BlockRef::Hash(h) => c.be.chain.block(h).map(|b| b.hash),
        BlockRef::Number(n) => {
            if n > c.be.chain.head().0 {
                return Ok(None);
            }
            c.be.chain.history(Class::Body, n)?;
            c.be.chain.hash_by_number(n)
        }
    })
}

/// `rpcOutputBlockSigners` (`api_xdc.go:292-338`): the recovered seals, not the sign transactions.
/// V1: the M2 validator seal, then the M1 creator seal; V2: the creator seal in `validator`. Never
/// split `validator` into 20-byte chunks. With no recoverable seal the miner field stands in.
fn block_signers(c: &Ctx, h: &Header, v2: bool) -> Vec<Address> {
    let mut out: Vec<Address> = vec![];
    let mut add = |a: Address| {
        if !out.contains(&a) {
            out.push(a);
        }
    };
    if v2 {
        if let Ok(a) = h.v2_signer() {
            add(a);
        }
    } else {
        if let Ok(a) = xone_primitives::ecrecover(&h.seal_hash(), &h.validator) {
            add(a);
        }
        if let Ok(a) = h.v1_signer() {
            add(a);
        }
    }
    let _ = c;
    if out.is_empty() {
        out.push(h.coinbase);
    }
    out
}

fn is_v2(c: &Ctx, number: u64) -> bool {
    c.be.chain
        .spec()
        .xdpos
        .v2
        .as_ref()
        .is_some_and(|v| number > v.switch_block)
}

/// A missing block, or a block with no masternodes, is the empty answer with no error.
fn signers(c: &Ctx, r: BlockRef) -> R {
    let Some(h) = resolve(c, r)? else {
        return Ok(json!([]));
    };
    let mn = c.be.consensus.masternodes(h)?;
    if mn.is_empty() {
        return Ok(json!([]));
    }
    let header =
        c.be.chain
            .header(h)
            .ok_or_else(|| RpcError::server("header not found"))?;
    Ok(Value::Array(
        block_signers(c, &header, is_v2(c, header.number))
            .into_iter()
            .map(addr)
            .collect(),
    ))
}

/// `findFinalityOfBlock` (`api_xdc.go:341-378`): 100 for a canonical V2 block at or below the
/// QC-committed one (F-2026-19638), else `signers * 100 / masternodes`.
fn finality(c: &Ctx, r: BlockRef) -> R {
    let Some(h) = resolve(c, r)? else {
        return Ok(json!(0));
    };
    let mn = c.be.consensus.masternodes(h)?;
    if mn.is_empty() {
        return Ok(json!(0));
    }
    let header =
        c.be.chain
            .header(h)
            .ok_or_else(|| RpcError::server("header not found"))?;
    let v2 = is_v2(c, header.number);
    if v2
        && c.be.chain.finalized().is_some_and(|f| header.number <= f)
        && c.be.chain.hash_by_number(header.number) == Some(h)
    {
        return Ok(json!(100));
    }
    let n = block_signers(c, &header, v2).len();
    Ok(json!(n * 100 / mn.len()))
}

/// The checkpoint of `epoch` (a block number or `latest`): `(n / E) * E`, or `E` for block 0.
fn checkpoint(c: &Ctx, p: &Params, i: usize) -> (u64, u64) {
    let e = c.be.chain.spec().xdpos.epoch.max(1);
    let n = p.number_or_latest(i).unwrap_or(0);
    let cp = n / e * e;
    let cp = if cp == 0 { e } else { cp };
    (cp, cp / e)
}

fn candidate_status(c: &Ctx, p: &Params) -> R {
    let who = p.address(0)?;
    let (cp, epoch) = checkpoint(c, p, 1);
    let mut m = json!({"status": "", "capacity": 0, "success": true, "epoch": epoch});
    let Some(h) = c.be.chain.hash_by_number(cp) else {
        m["success"] = json!(false);
        return Ok(m);
    };
    c.be.chain.history(Class::State, cp)?;
    let st = c.be.state.state(h)?;
    let st = Dyn(&*st);
    let list = vc::candidates(&st);
    if list.is_empty() {
        m["success"] = json!(false);
        return Ok(m);
    }
    let is_candidate = list.contains(&who);
    if is_candidate {
        m["status"] = json!("PROPOSED");
        m["capacity"] = bigint(vc::candidate_cap(&st, who));
    }
    let mn = c.be.consensus.masternodes(h)?;
    if mn.is_empty() {
        m["success"] = json!(false);
        return Ok(m);
    }
    if mn.contains(&who) {
        m["status"] = json!("MASTERNODE");
        if !is_candidate {
            m["capacity"] = json!(-1);
        }
    }
    Ok(m)
}

fn candidates(c: &Ctx, p: &Params) -> R {
    let (cp, epoch) = checkpoint(c, p, 0);
    let mut m = json!({"candidates": [], "success": true, "epoch": epoch});
    let h =
        c.be.chain
            .hash_by_number(cp)
            .ok_or_else(|| RpcError::server("header not found"))?;
    c.be.chain.history(Class::State, cp)?;
    let st = c.be.state.state(h)?;
    let st = Dyn(&*st);
    let mut rows: Vec<(U256, Address, Option<Address>)> = vc::candidates(&st)
        .into_iter()
        .map(|a| (vc::candidate_cap(&st, a), a, vc::candidate_owner(&st, a)))
        .collect();
    // `sort.Slice` by capacity descending; Go's sort is not stable, ties follow its pdqsort.
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    m["candidates"] = Value::Array(
        rows.into_iter()
            .map(|(cap, a, owner)| json!({"address": addr(a), "capacity": bigint(cap), "owner": owner.map_or_else(|| addr(Address::ZERO), addr)}))
            .collect(),
    );
    Ok(m)
}
