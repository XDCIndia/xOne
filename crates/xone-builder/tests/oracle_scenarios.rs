#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the vectors are trusted inputs and a panic is the failure signal"
)]
//! The pool against the pinned geth-XDC legacy pool: every scenario of
//! `conformance/vectors/txpool/scenarios.json` (built by `oracle/`, see its `run.sh`) is replayed, and after
//! every step the answer for each transaction and the whole content of the pool (executable and queued
//! hashes per sender, next nonces) must equal what geth reported.
mod common;

use alloy_primitives::{Address, U256};
use common::{account, vectors, MapState};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use xone_builder::txpool::{special::Authority, Config, Pool, PoolTx};
use xone_chainspec::Network;

#[derive(Deserialize)]
struct Acct {
    nonce: u64,
    balance: String,
    #[serde(default)]
    code: String,
}

#[derive(Deserialize, Default)]
struct Step {
    op: String,
    #[serde(default)]
    txs: Vec<String>,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    gas_limit: u64,
    #[serde(default)]
    accounts: BTreeMap<String, Acct>,
    #[serde(default)]
    addr: String,
    #[serde(default)]
    secs: u64,
    #[serde(default)]
    tip: u64,
    #[serde(default)]
    addrs: Vec<String>,
    #[serde(default)]
    syncing: bool,
}

#[derive(Deserialize)]
struct Expect {
    #[serde(default)]
    errs: Vec<String>,
    pending: BTreeMap<String, Vec<String>>,
    queued: BTreeMap<String, Vec<String>>,
    nonces: BTreeMap<String, u64>,
}

#[derive(Deserialize)]
struct Cfg {
    price_bump: u64,
    account_slots: u64,
    global_slots: u64,
    account_queue: u64,
    global_queue: u64,
    lifetime_secs: u64,
}

#[derive(Deserialize)]
struct Scenario {
    id: String,
    number: u64,
    gas_limit: u64,
    config: Cfg,
    min_tip: u64,
    syncing: bool,
    authorised: Option<Vec<String>>,
    accounts: BTreeMap<String, Acct>,
    steps: Vec<Step>,
    #[serde(default)]
    expect: Vec<Expect>,
}

#[derive(Deserialize)]
struct PinCrash {
    scenario: Scenario,
    pinned_pool: String,
}

#[derive(Deserialize)]
struct File {
    pin: String,
    scenarios: Vec<Scenario>,
    pin_crash: PinCrash,
}

fn addr(s: &str) -> Address {
    s.parse().unwrap()
}

fn state(accts: &BTreeMap<String, Acct>) -> MapState {
    let mut m = MapState::default();
    for (a, v) in accts {
        let code = if v.code.is_empty() {
            Vec::new()
        } else {
            hex(&v.code)
        };
        m.0.insert(
            addr(a),
            account(
                v.nonce,
                U256::from_str_radix(&v.balance, 10).unwrap(),
                &code,
            ),
        );
    }
    m
}

fn hex(s: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(s).unwrap()
}

fn authority(set: &[String], syncing: bool) -> Authority {
    if syncing {
        Authority::Syncing
    } else {
        Authority::candidates(set.iter().map(|a| addr(a)))
    }
}

fn names(r: Vec<Result<(), xone_builder::txpool::Error>>) -> Vec<String> {
    r.into_iter()
        .map(|x| x.err().map_or(String::new(), |e| e.name()))
        .collect()
}

fn hashes(m: BTreeMap<Address, Vec<std::sync::Arc<PoolTx>>>) -> BTreeMap<String, Vec<String>> {
    m.into_iter()
        .map(|(a, l)| {
            (
                format!("{a:#x}"),
                l.iter().map(|t| format!("{:#x}", t.hash)).collect(),
            )
        })
        .collect()
}

const NOW: u64 = 1_700_000_000;

fn load() -> File {
    serde_json::from_slice(&std::fs::read(vectors("scenarios.json")).unwrap()).unwrap()
}

/// Replay one scenario; with `compare`, check every step against geth's answer. Returns the pool, its
/// state and the number of steps.
fn replay(sc: &Scenario, compare: bool) -> (Pool, MapState, usize) {
    let cfg = Config {
        price_limit: sc.min_tip,
        price_bump: sc.config.price_bump,
        account_slots: sc.config.account_slots,
        global_slots: sc.config.global_slots,
        account_queue: sc.config.account_queue,
        global_queue: sc.config.global_queue,
        lifetime_secs: sc.config.lifetime_secs,
    };
    // geth's IsSigner is unset when a scenario names no signers and is not syncing; the pool has no unset
    // mode, so such a scenario must not carry a special transaction.
    let auth_set = sc.authorised.clone().unwrap_or_default();
    if sc.authorised.is_none() && !sc.syncing {
        for s in &sc.steps {
            for t in &s.txs {
                let t = PoolTx::decode(&hex(t), 51).unwrap();
                assert!(
                    !t.is_special(),
                    "{}: special transaction without a signer set",
                    sc.id
                );
            }
        }
    }
    let mut st = state(&sc.accounts);
    let mut pool = Pool::new(
        Network::Apothem.spec(),
        &cfg,
        authority(&auth_set, sc.syncing),
        sc.number,
        sc.gas_limit,
    )
    .unwrap();
    let mut watch: BTreeSet<Address> = sc.accounts.keys().map(|a| addr(a)).collect();
    for (i, step) in sc.steps.iter().enumerate() {
        let ctx = format!("{} step {i} ({})", sc.id, step.op);
        let mut errs = Vec::new();
        match step.op.as_str() {
            "add" => {
                let raws: Vec<Vec<u8>> = step.txs.iter().map(|t| hex(t)).collect();
                for r in &raws {
                    if let Ok(t) = PoolTx::decode(r, 51) {
                        watch.insert(t.from());
                    }
                }
                let refs: Vec<&[u8]> = raws.iter().map(Vec::as_slice).collect();
                errs = names(pool.add_raw(&st, NOW, &refs));
            }
            "reset" => {
                st = state(&step.accounts);
                watch.extend(step.accounts.keys().map(|a| addr(a)));
                pool.reset(&st, NOW, step.number, step.gas_limit).unwrap();
            }
            "age" => pool.set_queue_age(NOW, addr(&step.addr), step.secs),
            "evict" => {
                pool.evict_expired(&st, NOW);
            }
            "set_gas_tip" => pool.set_gas_tip(&st, u128::from(step.tip)),
            "signers" => pool.set_authority(authority(&step.addrs, step.syncing)),
            other => panic!("op {other}"),
        }
        // The lookup and the lists agree, whatever the oracle says.
        let (pp, qq) = pool.stats();
        assert_eq!(pool.len().0, pp + qq, "{ctx}: lookup and lists agree");
        if !compare {
            continue;
        }
        let want = &sc.expect[i];
        assert_eq!(errs, want.errs, "{ctx}: answers");
        let (p, q) = pool.content();
        assert_eq!(hashes(p), want.pending, "{ctx}: executable");
        assert_eq!(hashes(q), want.queued, "{ctx}: queued");
        let mut have: BTreeMap<String, u64> = BTreeMap::new();
        for a in watch.iter().chain(pool.senders().iter()) {
            let n = pool.nonce(&st, *a);
            if n != 0 {
                have.insert(format!("{a:#x}"), n);
            }
        }
        assert_eq!(have, want.nonces, "{ctx}: next nonces");
    }
    (pool, st, sc.steps.len())
}

#[test]
fn every_scenario_equals_the_pinned_geth_pool() {
    let f = load();
    assert_eq!(f.pin, "6fc8e8c2088b7f18264f5663036669d04fa7becf");
    let mut steps = 0;
    for sc in &f.scenarios {
        assert_eq!(sc.expect.len(), sc.steps.len(), "{}", sc.id);
        steps += replay(sc, true).2;
    }
    let scripted = f
        .scenarios
        .iter()
        .filter(|s| !s.id.starts_with("rnd-"))
        .count();
    eprintln!(
        "txpool oracle: {} scenarios ({scripted} scripted, {} seeded), {steps} steps, equal to geth after every step",
        f.scenarios.len(),
        f.scenarios.len() - scripted
    );
    assert!(scripted >= 50, "{scripted} scripted scenarios");
}

/// geth leaves the cost of a fast-pathed special transaction out of the list total and panics when the
/// block that includes it removes it (`pin_crash`, from `oracle/run.sh`). The pool must survive the same
/// script and end empty.
#[test]
fn pinned_pool_crash_on_included_sign_tx_with_value_does_not_crash_here() {
    let f = load();
    assert!(
        f.pin_crash
            .pinned_pool
            .starts_with("crashed, panic: totalcost underflow"),
        "{}",
        f.pin_crash.pinned_pool
    );
    let (pool, _, steps) = replay(&f.pin_crash.scenario, false);
    assert_eq!(steps, 2);
    assert!(pool.is_empty(), "the included transaction is gone");
}
