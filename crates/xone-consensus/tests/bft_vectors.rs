#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The V2 BFT vectors of `conformance/vectors/v2/bft-*.json` (`README.md` there): inputs and expected
//! values come from the pinned geth-XDC engine (`crates/xone-consensus/oracle/bft`: `timer`, `rule`,
//! `forensics`). Nothing is typed by hand.

use std::{collections::HashMap, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256};
use serde::Deserialize;
use xone_chainspec::{
    apothem, devnet, devnet551, load_named, mainnet, ExpTimeoutConfig, V2Config, V2,
};
use xone_consensus::v2::{
    bft::{
        rule::{verify_voting_rule, Verdict, Why},
        Chain, EpochFacts, HeaderFacts,
    },
    cert::Secp,
    forensics::Forensics,
    safety::BlockRef,
    timer::Timer,
    Error,
};
use xone_primitives::{BlockInfo, QuorumCert};

fn read<T: for<'de> Deserialize<'de>>(file: &str) -> T {
    let path = format!(
        "{}/../../conformance/vectors/v2/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap()
}

// ---------------------------------------------------------------- timer

#[derive(Deserialize)]
struct Row {
    key: u64,
    timeout_period: u64,
    base: f64,
    max_exponent: u8,
    timeout_sync_threshold: u64,
    mine_period: u64,
}

#[derive(Deserialize)]
struct Step {
    round: u64,
    qc_round: u64,
    delays_ns: Vec<u64>,
}

#[derive(Deserialize)]
struct TimerSet {
    name: String,
    rows: Vec<Row>,
    initial_ns: u64,
    steps: Vec<Step>,
}

fn v2_of(rows: &[Row]) -> V2 {
    V2 {
        switch_block: 0,
        switch_epoch: 1,
        configs: rows
            .iter()
            .map(|r| {
                (
                    r.key,
                    V2Config {
                        timeout_period: r.timeout_period,
                        timeout_sync_threshold: r.timeout_sync_threshold,
                        mine_period: r.mine_period,
                        exp_timeout: ExpTimeoutConfig {
                            base: r.base,
                            max_exponent: r.max_exponent,
                        },
                        ..V2Config::default()
                    },
                )
            })
            .collect(),
    }
}

/// The V2 section of a built-in network, for the rows the oracle read from `forks.json`.
fn spec_v2(name: &str) -> Option<V2> {
    let spec = match name {
        "mainnet" => mainnet(),
        "apothem" => apothem(),
        "devnet" => devnet(),
        "devnet551" => devnet551(),
        "net2040" => {
            let p = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/vectors/consensus/net2040.forks.json"
            );
            load_named("net2040", &std::fs::read_to_string(p).unwrap()).unwrap()
        }
        _ => return None,
    };
    spec.xdpos.v2
}

#[test]
fn the_timeout_schedule_equals_geth_for_every_config_row() {
    let sets: Vec<TimerSet> = read("bft-timer.json");
    let (mut real_rows, mut steps, mut distinct) = (0, 0, std::collections::BTreeSet::new());
    for s in &sets {
        // the rows the oracle ran are the rows of the chain spec, or the synthetic ones of the vector
        let v2 = match spec_v2(&s.name) {
            Some(spec) => {
                assert_eq!(spec.configs.len(), s.rows.len(), "{}", s.name);
                for r in &s.rows {
                    let c = &spec.configs[&r.key];
                    assert_eq!(
                        (
                            c.timeout_period,
                            c.exp_timeout.base,
                            c.exp_timeout.max_exponent,
                            c.timeout_sync_threshold,
                            c.mine_period
                        ),
                        (
                            r.timeout_period,
                            r.base,
                            r.max_exponent,
                            r.timeout_sync_threshold,
                            r.mine_period
                        ),
                        "{} row {}",
                        s.name,
                        r.key
                    );
                    real_rows += 1;
                }
                spec
            }
            None => v2_of(&s.rows),
        };
        let mut t = Timer::new(v2.config_at_round(0).unwrap());
        // `initial` ends with Reset(0, 0)
        assert_eq!(
            t.reset(0, 0).1,
            Duration::from_nanos(s.initial_ns),
            "{} initial",
            s.name
        );
        for st in &s.steps {
            // setNewRound: the config of the round, then Reset(round, round of the highest QC)
            t.set_params(v2.config_at_round(st.round).unwrap());
            let mut got = vec![t.reset(st.round, st.qc_round).1];
            while got.len() < st.delays_ns.len() {
                got.push(t.fire().1);
            }
            let want: Vec<_> = st
                .delays_ns
                .iter()
                .map(|n| Duration::from_nanos(*n))
                .collect();
            assert_eq!(
                got, want,
                "{} round {} qc {}",
                s.name, st.round, st.qc_round
            );
            distinct.extend(st.delays_ns.iter().copied());
            steps += 1;
        }
    }
    eprintln!(
        "{} sets, {real_rows} real rows, {steps} steps, {} distinct delays",
        sets.len(),
        distinct.len()
    );
    assert!(
        real_rows > 7 + 5,
        "mainnet, apothem and net2040 rows are all in the vector"
    );
    assert!(
        distinct.len() > 40,
        "the exponential schedules are exercised"
    );
}

// ---------------------------------------------------------------- rule

#[derive(Deserialize)]
struct Hdr {
    hash: B256,
    number: u64,
    parent: B256,
}

#[derive(Deserialize, Clone)]
struct Ref {
    hash: B256,
    round: u64,
    number: u64,
}

#[derive(Deserialize)]
struct RuleCase {
    id: String,
    round: u64,
    highest_voted_round: u64,
    lock: Option<Ref>,
    block: Ref,
    qc_round: u64,
    vote: bool,
    err: Option<String>,
}

#[derive(Deserialize)]
struct Sibling {
    headers: Vec<Hdr>,
    names: Vec<String>,
    cases: Vec<RuleCase>,
}

#[derive(Deserialize)]
struct RuleOut {
    headers: Vec<Hdr>,
    cases: Vec<RuleCase>,
    sibling: Sibling,
}

/// Headers by hash; only `header` is answered.
struct Headers(HashMap<B256, Arc<HeaderFacts>>);

impl Headers {
    fn of(l: &[Hdr]) -> Self {
        Self(
            l.iter()
                .map(|h| {
                    (
                        h.hash,
                        Arc::new(HeaderFacts {
                            number: h.number,
                            hash: h.hash,
                            parent: h.parent,
                            round: 0,
                            qc: None,
                        }),
                    )
                })
                .collect(),
        )
    }
}

impl Chain for Headers {
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>> {
        self.0.get(hash).cloned()
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        None
    }
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, Error> {
        Err(Error::MissingHeader { hash: *hash })
    }
    fn epoch_of_round(&self, round: u64) -> Result<Arc<EpochFacts>, Error> {
        Err(Error::NoV2Config(round))
    }
    fn candidates(&self, _: u64) -> Option<Arc<[Address]>> {
        None
    }
    fn timeout_gap(&self, round: u64) -> Result<u64, Error> {
        Err(Error::NoV2Config(round))
    }
}

fn decide(c: &Headers, k: &RuleCase) -> Result<Verdict, Error> {
    let lock = k.lock.as_ref().map(|l| BlockRef {
        hash: l.hash,
        round: l.round,
        number: l.number,
    });
    let block = BlockInfo {
        hash: k.block.hash,
        round: k.block.round,
        number: k.block.number,
    };
    verify_voting_rule(
        c,
        k.round,
        k.highest_voted_round,
        lock.as_ref(),
        &block,
        k.qc_round,
    )
}

fn check_rule(c: &Headers, cases: &[RuleCase]) -> [usize; 7] {
    // votes by the lock, votes by extending the lock, no vote: voted, round, lock; errors
    let mut seen = [0; 7];
    for k in cases {
        let got = decide(c, k);
        match (&got, k.vote, &k.err) {
            (Ok(Verdict::Vote { extends_lock }), true, None) => {
                seen[usize::from(*extends_lock)] += 1;
                if k.lock.as_ref().is_none_or(|l| k.qc_round > l.round) {
                    assert!(!extends_lock, "{}", k.id);
                }
            }
            (Ok(Verdict::No(why)), false, None) => {
                seen[2 + match why {
                    Why::AlreadyVoted => 0,
                    Why::RoundMismatch => 1,
                    Why::NotExtended => 2,
                }] += 1;
            }
            (Err(_), false, Some(_)) => seen[5] += 1,
            _ => panic!(
                "{}: oracle vote={} err={:?}, got {got:?}",
                k.id, k.vote, k.err
            ),
        }
    }
    seen
}

#[test]
fn the_voting_rule_decides_as_geth_on_explicit_inputs() {
    let v: RuleOut = read("bft-rule.json");
    let c = Headers::of(&v.headers);
    let seen = check_rule(&c, &v.cases);
    eprintln!(
        "{} cases: lock-above {} extends {} | no vote: voted {} round {} lock {} | errors {}",
        v.cases.len(),
        seen[0],
        seen[1],
        seen[2],
        seen[3],
        seen[4],
        seen[5]
    );
    assert!(
        seen[..6].iter().all(|n| *n >= 10),
        "every branch of the rule is exercised: {seen:?}"
    );
}

#[test]
fn the_sibling_shape_of_net2040_619712_619713_is_voted_as_geth() {
    let v: RuleOut = read("bft-rule.json");
    let s = &v.sibling;
    let c = Headers::of(&s.headers);
    check_rule(&c, &s.cases);
    let by = |name: &str| s.cases.iter().find(|k| k.id.starts_with(name)).unwrap();
    // the heights of the two siblings are those of the incident, and they share a parent
    let n = |name: &str| s.headers[s.names.iter().position(|x| x == name).unwrap()].number;
    assert_eq!(
        (n("A"), n("A'"), n("child of A'")),
        (619_712, 619_712, 619_713)
    );
    assert!(
        by("A' after the TC").vote,
        "the sibling of a timed-out block is voted in the next round"
    );
    assert!(
        !by("A' arrives before the TC").vote,
        "and not before the TC: the machine keeps it"
    );
    assert!(
        !by("A'' after A' was voted").vote,
        "a second sibling in the same round is not voted"
    );
    assert!(by("child of A'").vote);
}

// ---------------------------------------------------------------- forensics

#[derive(Deserialize, Clone)]
struct QcRec {
    hash: B256,
    round: u64,
    number: u64,
    gap: u64,
}

#[derive(Deserialize)]
struct FHdr {
    hash: B256,
    number: u64,
    parent: B256,
    round: u64,
    qc: QcRec,
    switch: B256,
}

#[derive(Deserialize)]
struct Commit {
    parent: B256,
    proposed: B256,
    incoming: QcRec,
}

#[derive(Deserialize)]
struct Proof {
    id: String,
    diverging_hash: B256,
    diverging_number: u64,
    across_epoch: bool,
    smaller_path: Vec<B256>,
    larger_path: Vec<B256>,
    smaller_qc_hash: B256,
    larger_qc_hash: B256,
}

#[derive(Deserialize)]
struct FCase {
    id: String,
    first: Commit,
    second: Commit,
    proofs: Option<Vec<Proof>>,
    first_proofs: Option<Vec<Proof>>,
}

#[derive(Deserialize)]
struct FOut {
    chains: Vec<Vec<FHdr>>,
    cases: Vec<FCase>,
}

struct FChain(HashMap<B256, (Arc<HeaderFacts>, B256)>);

impl Chain for FChain {
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>> {
        self.0.get(hash).map(|x| x.0.clone())
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        None
    }
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, Error> {
        let (_, switch) = self
            .0
            .get(hash)
            .ok_or(Error::MissingHeader { hash: *hash })?;
        Ok(Arc::new(EpochFacts {
            masternodes: vec![],
            gap: 0,
            switch: *switch,
        }))
    }
    fn epoch_of_round(&self, round: u64) -> Result<Arc<EpochFacts>, Error> {
        Err(Error::NoV2Config(round))
    }
    fn candidates(&self, _: u64) -> Option<Arc<[Address]>> {
        None
    }
    fn timeout_gap(&self, round: u64) -> Result<u64, Error> {
        Err(Error::NoV2Config(round))
    }
}

fn qc_of(q: &QcRec) -> QuorumCert {
    QuorumCert {
        proposed_block_info: BlockInfo {
            hash: q.hash,
            round: q.round,
            number: q.number,
        },
        signatures: vec![],
        gap_number: q.gap,
    }
}

fn monitor(
    f: &mut Forensics,
    c: &FChain,
    k: &Commit,
) -> Option<xone_consensus::v2::forensics::Proof> {
    let (p, h) = (&c.0[&k.parent].0, &c.0[&k.proposed].0);
    f.monitor(c, &Secp, p, h, &qc_of(&k.incoming))
}

#[test]
fn two_conflicting_qcs_make_one_event_with_the_id_of_geth() {
    let v: FOut = read("bft-forensics.json");
    let (mut events, mut quiet, mut across) = (0, 0, 0);
    for (case, chain) in v.cases.iter().zip(&v.chains) {
        let c = FChain(
            chain
                .iter()
                .map(|h| {
                    (
                        h.hash,
                        (
                            Arc::new(HeaderFacts {
                                number: h.number,
                                hash: h.hash,
                                parent: h.parent,
                                round: h.round,
                                qc: Some(qc_of(&h.qc)),
                            }),
                            h.switch,
                        ),
                    )
                })
                .collect(),
        );
        let mut f = Forensics::default();
        assert!(
            monitor(&mut f, &c, &case.first).is_none(),
            "{}: the first commit has nothing to conflict with",
            case.id
        );
        assert!(case.first_proofs.is_none());
        let got = monitor(&mut f, &c, &case.second);
        match (got, case.proofs.as_deref()) {
            (None, None) => quiet += 1,
            (Some(g), Some([w])) => {
                assert_eq!(g.id, w.id, "{}", case.id);
                assert_eq!(g.diverging_hash, w.diverging_hash, "{}", case.id);
                assert_eq!(g.diverging_number, w.diverging_number, "{}", case.id);
                assert_eq!(g.across_epoch, w.across_epoch, "{}", case.id);
                assert_eq!(g.smaller.path, w.smaller_path, "{}", case.id);
                assert_eq!(g.larger.path, w.larger_path, "{}", case.id);
                assert_eq!(
                    g.smaller.qc.proposed_block_info.hash, w.smaller_qc_hash,
                    "{}",
                    case.id
                );
                assert_eq!(
                    g.larger.qc.proposed_block_info.hash, w.larger_qc_hash,
                    "{}",
                    case.id
                );
                events += 1;
                across += usize::from(w.across_epoch);
                // the same conflict a second time is not reported again
                let again = monitor(&mut f, &c, &case.second);
                assert!(again.is_none(), "{}: one event per id", case.id);
            }
            (g, w) => panic!(
                "{}: oracle {:?}, got {:?}",
                case.id,
                w.map(|p| p.len()),
                g.map(|p| p.id)
            ),
        }
    }
    eprintln!(
        "{} cases: {events} events ({across} across epochs), {quiet} none",
        v.cases.len()
    );
    assert!(events > 50 && quiet > 10 && across > 5);
}
