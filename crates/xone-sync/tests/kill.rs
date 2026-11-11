#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! Crash and failure at every ADR-005 kill point (K01-K19, U1a-U5b) during insert, reorg, sibling
//! adoption and set-head; 100 randomized restart sequences; `kill -9` during a sync-like burst.
//! After every crash the datadir is opened by `Chain::open` (reconcile, repair, head checks) with no
//! other help. The recovered head must equal the head the store had published when it died, the state
//! must be that head's state, the canonical index must link back to the anchor, and the work must be
//! finishable by importing again. Nothing is ever wiped.

mod common;
use common::*;
use std::collections::BTreeSet;
use xone_db::Durability;
use xone_sync::{
    chain::{Chain, Resumed},
    store::{KillPoint, Store},
    testkit::{Cut, Fault, MemStore},
};

enum Op {
    Insert(&'static str),
    SetHead(u64),
}

/// A start state, one operation, and how to finish it after a restart.
struct Scn {
    name: &'static str,
    v2: bool,
    fork: u64,
    labels: &'static [&'static str],
    prepare: &'static [&'static str],
    op: Op,
    /// Blocks inserted again after the restart, in order.
    finish: &'static [&'static str],
    /// `set-head` target to repeat after the restart when the head is above it.
    rewind: Option<u64>,
    final_head: &'static str,
}

const SCENARIOS: [Scn; 7] = [
    Scn {
        name: "insert",
        v2: true,
        fork: 0,
        labels: &["m1", "m2", "m3", "m4"],
        prepare: &["m1", "m2", "m3"],
        op: Op::Insert("m4"),
        finish: &["m4"],
        rewind: None,
        final_head: "m4",
    },
    Scn {
        name: "insert-v1",
        v2: false,
        fork: 0,
        labels: &["m1", "m2", "m3", "m4", "m5"],
        prepare: &["m1", "m2", "m3", "m4"],
        op: Op::Insert("m5"),
        finish: &["m5"],
        rewind: None,
        final_head: "m5",
    },
    Scn {
        name: "reorg",
        v2: true,
        fork: 2,
        labels: &["s1", "s2", "m3", "m4", "m5", "a3", "a4", "a5", "a6"],
        prepare: &["s1", "s2", "m3", "m4", "m5", "a3", "a4", "a5"],
        op: Op::Insert("a6"),
        finish: &["a3", "a4", "a5", "a6"],
        rewind: None,
        final_head: "a6",
    },
    Scn {
        name: "reorg-v1",
        v2: false,
        fork: 2,
        labels: &["s1", "s2", "m3", "m4", "a3", "a4"],
        prepare: &["s1", "s2", "m3", "m4"],
        op: Op::Insert("a3"),
        finish: &["a3"],
        rewind: None,
        final_head: "a3",
    },
    Scn {
        name: "sibling-adoption",
        v2: true,
        fork: 3,
        labels: &["s1", "s2", "s3", "m4", "m5", "a4"],
        prepare: &["s1", "s2", "s3", "a4", "m4"],
        op: Op::Insert("m5"),
        finish: &["m4", "m5"],
        rewind: None,
        final_head: "m5",
    },
    Scn {
        name: "set-head",
        v2: true,
        fork: 0,
        labels: &["m1", "m2", "m3", "m4", "m5", "m6"],
        prepare: &["m1", "m2", "m3", "m4", "m5", "m6"],
        op: Op::SetHead(3),
        finish: &["m4", "m5", "m6"],
        rewind: Some(3),
        final_head: "m6",
    },
    Scn {
        name: "set-head-v1",
        v2: false,
        fork: 0,
        labels: &["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"],
        prepare: &["m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8"],
        op: Op::SetHead(2),
        finish: &["m3", "m4", "m5", "m6", "m7", "m8"],
        rewind: Some(2),
        final_head: "m8",
    },
];

/// A world built once: every chain of a test gets its own copy of the spec and the recordings.
struct Parts {
    spec: xone_chainspec::ChainSpec,
    exec: xone_sync::testkit::VectorExecutor,
    t: Tree,
    bpf: u64,
}

impl Parts {
    fn new(
        v2: bool,
        fork: u64,
        labels: impl IntoIterator<Item = impl AsRef<str>>,
        bpf: u64,
    ) -> Self {
        let mut w = World::new(v2, fork);
        for l in labels {
            w.make(l.as_ref());
        }
        Self {
            spec: w.spec,
            exec: w.exec,
            t: Tree {
                blocks: w.blocks,
                touched: w.touched,
            },
            bpf,
        }
    }

    fn chain(&self, store: MemStore) -> TestChain {
        Chain::new(
            self.spec.clone(),
            store,
            self.exec.clone(),
            accept as Accept,
        )
    }

    fn fresh(&self) -> MemStore {
        let g = &self.t.blocks["s0"];
        MemStore::with_anchor(g.block.header.clone(), Default::default(), 64, self.bpf)
    }
}

fn parts(s: &Scn) -> Parts {
    Parts::new(s.v2, s.fork, s.labels, 4)
}

fn start(p: &Parts, s: &Scn) -> TestChain {
    let chain = p.chain(p.fresh());
    for l in s.prepare {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    chain
}

fn label_of(t: &Tree, hash: alloy_primitives::B256) -> String {
    t.blocks
        .iter()
        .find(|(_, g)| g.block.hash() == hash)
        .map(|(l, _)| l.clone())
        .expect("a head that is not a known block")
}

/// The invariants of a consistent datadir: the head has its state, the index links to the anchor,
/// the floor is a canonical block.
fn consistent(chain: &TestChain, t: &Tree) -> String {
    use xone_sync::chain::ChainRead;
    let (n, h) = chain.head();
    let label = label_of(t, h);
    t.assert_state(chain, &label);
    let r = chain.reader();
    let mut child = h;
    for k in (0..=n).rev() {
        let c = r
            .canonical_hash(k)
            .unwrap_or_else(|| panic!("no canonical hash for #{k}"));
        assert_eq!(c, child, "index links at #{k}");
        child = r.header(c).unwrap().parent_hash;
    }
    assert_eq!(r.canonical_hash(n + 1), None);
    if let Some(f) = r.committed() {
        assert_eq!(
            r.canonical_hash(f.number),
            Some(f.hash),
            "the committed block is canonical"
        );
    }
    label
}

fn reopen(
    p: &Parts,
    store: MemStore,
    cut: Cut,
) -> Result<(TestChain, Resumed), xone_sync::chain::ResumeError> {
    Chain::open(
        p.spec.clone(),
        MemStore::open(store.crash(cut)),
        p.exec.clone(),
        accept as Accept,
    )
}

fn finish(s: &Scn, chain: &TestChain, t: &Tree) {
    if let Some(r) = s.rewind {
        if chain.head().0 > r {
            chain.set_head(r).unwrap();
        }
    }
    for l in s.finish {
        chain
            .insert(t.block(l), 0)
            .unwrap_or_else(|e| panic!("{}: finishing {l}: {e}", s.name));
    }
    let label = consistent(chain, t);
    assert_eq!(label, s.final_head, "{}", s.name);
}

#[test]
fn every_kill_point_every_operation() {
    let mut hit: BTreeSet<KillPoint> = BTreeSet::new();
    let mut runs = 0;
    let faults = [
        (Fault::Kill, None),
        (Fault::Power, None),
        (Fault::Power, Some(Cut::Torn(7))),
        (Fault::Io, None),
    ];
    for s in &SCENARIOS {
        let p = parts(s);
        for kp in KillPoint::ALL {
            for (fault, cut) in faults
                .iter()
                .copied()
                .chain((kp == KillPoint::K15).then_some((Fault::IoCommitted, None)))
            {
                for skip in 0.. {
                    let mut store = start(&p, s).into_store();
                    store.arm(kp, fault, skip);
                    let chain = p.chain(store);
                    let r = match s.op {
                        Op::Insert(l) => chain
                            .insert(p.t.block(l), 0)
                            .map(|_| ())
                            .map_err(|e| e.to_string()),
                        Op::SetHead(n) => chain.set_head(n).map_err(|e| e.to_string()),
                    };
                    let store = chain.into_store();
                    if store.durability() == Durability::Durable {
                        assert!(r.is_ok(), "{}: {r:?}", s.name);
                        break; // the point is not reached again in this operation
                    }
                    assert!(
                        r.is_err(),
                        "{} {kp:?}: a dead store reported success",
                        s.name
                    );
                    assert!(!store.durability().may_sign());
                    assert!(store.log().contains(&kp));
                    hit.insert(kp);
                    let published = store.head();
                    let cut = cut.unwrap_or_else(|| store.natural_cut());
                    let what = format!("{} {kp:?} {fault:?} cut {cut:?} skip {skip}", s.name);
                    let (chain, resumed) = reopen(&p, store, cut)
                        .unwrap_or_else(|e| panic!("{what}: restart refused: {e}"));
                    assert!(matches!(resumed, Resumed::Head { .. }), "{what}");
                    assert_eq!(chain.head(), published, "{what}: head after restart");
                    consistent(&chain, &p.t);
                    finish(s, &chain, &p.t);
                    let store = chain.into_store();
                    assert_eq!(
                        store.flat_root(),
                        p.t.blocks[s.final_head].block.header.state_root,
                        "{what}: state root"
                    );
                    runs += 1;
                }
            }
        }
    }
    let missed: Vec<_> = KillPoint::ALL.iter().filter(|k| !hit.contains(k)).collect();
    assert!(missed.is_empty(), "kill points never reached: {missed:?}");
    println!(
        "kill points: {} points x {} operations, {runs} crash runs, all recovered without repair",
        hit.len(),
        SCENARIOS.len()
    );
}

#[test]
fn a_kill_at_a_commit_loses_nothing_that_was_published() {
    // K15 with an unknown outcome: the manifest decides, and the head is either side of the commit.
    let s = &SCENARIOS[0];
    let p = parts(s);
    let mut store = start(&p, s).into_store();
    let before = store.head();
    store.arm(KillPoint::K15, Fault::IoCommitted, 0);
    let chain = p.chain(store);
    assert!(chain.insert(p.t.block("m4"), 0).is_err());
    let after = chain.head();
    assert_ne!(before, after, "the commit was applied before the error");
    let (chain, _) = reopen(&p, chain.into_store(), Cut::PowerLoss).unwrap();
    assert_eq!(chain.head(), after);
}

fn seq(p: &Parts, labels: &[String], seed: u64) -> usize {
    let mut crashes = 0;
    let mut rng = Rng(seed);
    let mut chain = p.chain(p.fresh());
    let (mut next_m, mut next_a) = (3u64, 3u64);
    for n in 1..=2 {
        chain.insert(p.t.block(&format!("s{n}")), 0).unwrap();
    }
    for step in 0..20 {
        let mut store = chain.into_store();
        // the first step is always an insert that dies at a commit point: every sequence restarts at least once
        if step == 0 || rng.below(4) != 0 {
            // commit points are in every insert; the unwind points only in a reorg or a set-head
            let range = if step > 0 && rng.below(4) == 0 {
                27
            } else {
                19
            };
            let mut kp = KillPoint::ALL[rng.below(range) as usize];
            if step == 0 && kp == KillPoint::K09 {
                kp = KillPoint::K10; // K09 needs a chunk boundary
            }
            let fault = [Fault::Kill, Fault::Power, Fault::Io][rng.below(3) as usize];
            store.arm(kp, fault, if step == 0 { 0 } else { rng.below(3) as usize });
        }
        chain = p.chain(store);
        match if step == 0 { 0 } else { rng.below(4) } {
            0 | 1 if next_m <= 12 => {
                let _ = chain.insert(p.t.block(&format!("m{next_m}")), 0);
                next_m += 1;
            }
            2 if next_a <= 9 => {
                let _ = chain.insert(p.t.block(&format!("a{next_a}")), 0);
                next_a += 1;
            }
            _ => {
                let (h, f) = (chain.head().0, chain.committed().map_or(0, |l| l.number));
                let _ = chain.set_head(h.saturating_sub(rng.below(4)).max(f));
            }
        }
        let store = chain.into_store();
        if store.durability() == Durability::Durable {
            chain = p.chain(store);
            consistent(&chain, &p.t);
            continue;
        }
        crashes += 1;
        let published = store.head();
        let cut = store.natural_cut();
        let (c, r) =
            reopen(p, store, cut).unwrap_or_else(|e| panic!("seed {seed}: restart refused: {e}"));
        assert!(matches!(r, Resumed::Head { .. }), "seed {seed}");
        assert_eq!(c.head(), published, "seed {seed}: head after restart");
        consistent(&c, &p.t);
        chain = c;
        // The next steps re-feed what may have been lost.
        next_m = next_m.saturating_sub(1).max(3);
        next_a = next_a.saturating_sub(1).max(3);
    }
    // Import everything that is left, in order: the datadir takes it with no repair.
    for l in labels {
        let _ = chain.insert(p.t.block(l), 0);
    }
    consistent(&chain, &p.t);
    assert!(crashes >= 1, "seed {seed}: no crash happened");
    crashes
}

#[test]
fn a_hundred_randomized_restart_sequences_resume_with_the_correct_head() {
    let labels: Vec<String> = (1..=2)
        .map(|n| format!("s{n}"))
        .chain((3..=12).map(|n| format!("m{n}")))
        .chain((3..=9).map(|n| format!("a{n}")))
        .collect();
    let (v1, v2) = (
        Parts::new(false, 2, &labels, 4),
        Parts::new(true, 2, &labels, 4),
    );
    let mut crashes = 0;
    for seed in 0..100 {
        crashes += seq(
            if seed % 2 == 0 { &v2 } else { &v1 },
            &labels,
            0x5EED_0000 + seed,
        );
    }
    assert!(crashes >= 200, "only {crashes} crashes in 100 sequences");
    println!("100 randomized sequences, {crashes} crash-restarts, 0 repairs by hand");
}

/// `kill -9` ten times during an import burst; each restart rolls back to the last published
/// head and the burst goes on. The datadir is the same object throughout: nothing is wiped.
#[test]
fn kill9_during_a_sync_burst_recovers_by_rollback_never_by_wipe() {
    let labels: Vec<String> = (1..=48).map(|n| format!("m{n}")).collect();
    for v2 in [true, false] {
        let p = Parts::new(v2, 0, &labels, 8);
        let mut chain = p.chain(p.fresh());
        for l in &labels[..5] {
            chain.insert(p.t.block(l), 0).unwrap();
        }
        // ten points: journal, execute, each segment, chunk creation, sync, txn, commit, publish
        let points = [
            KillPoint::K02,
            KillPoint::K05,
            KillPoint::K07,
            KillPoint::K09,
            KillPoint::K11,
            KillPoint::K13,
            KillPoint::K14,
            KillPoint::K15,
            KillPoint::K17,
            KillPoint::K18,
        ];
        for (i, kp) in points.iter().enumerate() {
            let mut store = chain.into_store();
            store.arm(*kp, Fault::Kill, 2 * (i % 3));
            chain = p.chain(store);
            let died = labels
                .iter()
                .any(|l| chain.insert(p.t.block(l), 0).is_err());
            assert!(died, "{kp:?} was not reached in the burst");
            let store = chain.into_store();
            let (published, before) = (store.head(), store.canonical_numbers());
            let (c, r) = reopen(&p, store, Cut::Kill9).unwrap();
            assert!(
                matches!(r, Resumed::Head { number, .. } if number >= 5),
                "restart below the work already done"
            );
            assert_eq!(c.head(), published);
            // rollback never by wipe: the index is exactly what the dead process had published
            let store = c.into_store();
            assert_eq!(store.canonical_numbers(), before);
            chain = p.chain(store);
        }
        for l in &labels {
            chain.insert(p.t.block(l), 0).unwrap();
        }
        assert_eq!(chain.head().1, p.t.hash("m48"));
        consistent(&chain, &p.t);
        assert_eq!(
            chain.into_store().flat_root(),
            p.t.blocks["m48"].block.header.state_root
        );
    }
}
