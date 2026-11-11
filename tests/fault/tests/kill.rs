#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The ADR-005 kill matrix on the real store (tests/fault/PLAN.md sections 1, 2 and 5): a kill at
//! every point K01-K19 and U1a-U5b during insert, reorg, sibling adoption and set-head, then every
//! crash image of the disk the faults F1 to F5 can leave (`Pick`), and the failures F6 to F8.
//! MDBX is real, in a scratch directory; the segments and the journal are on the in-memory disk.
//! After each image the restart is `Chain::open` with no help, and it is held to the oracle of
//! PLAN.md section 2: one committed generation, a typed row of the decision table, every block
//! readable with a valid CRC, nothing served beyond the manifest, and the work finishable.

mod support;
use std::collections::{BTreeMap, BTreeSet};
use support::{crash::*, *};
use xone_db::{
    fault::{Errno, MemDir, Pick},
    Cause, Durability,
};
use xone_fault::{Arm, Scratch};
use xone_sync::{
    chain::Resumed,
    mdbx::{MdbxStore, OpenError},
    store::{KillPoint, Store},
    testkit::Fault,
};

enum Op {
    Insert(&'static str),
    SetHead(u64),
}

struct Scn {
    name: &'static str,
    v2: bool,
    fork: u64,
    labels: &'static [&'static str],
    prepare: &'static [&'static str],
    op: Op,
    finish: &'static [&'static str],
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

fn seeds() -> u64 {
    std::env::var("XONE_FAULT_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
}

fn parts(s: &Scn) -> Parts {
    Parts::new(s.v2, s.fork, s.labels, 4)
}

fn start(p: &Parts, s: &Scn, sc: &Scratch, dir: MemDir) -> MChain {
    let chain = p.chain(p.fresh_on(sc, dir));
    for l in s.prepare {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    chain
}

fn run(p: &Parts, s: &Scn, chain: &MChain) -> Result<(), String> {
    match s.op {
        Op::Insert(l) => chain
            .insert(p.t.block(l), 0)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Op::SetHead(n) => chain.set_head(n).map_err(|e| e.to_string()),
    }
}

fn finish(s: &Scn, chain: &MChain, t: &Tree) {
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
    assert_eq!(consistent(chain, t), s.final_head, "{}", s.name);
}

/// The rows the decision table allows after a kill at `kp` (PLAN.md section 1).
fn allowed(kp: KillPoint) -> BTreeSet<Row> {
    use KillPoint::*;
    match kp {
        K06 | K07 | K08 | K09 | K10 | K11 | K12 | K13 | K14 | K15 => {
            set([Row::Ready, Row::D12, Row::D13])
        }
        U3b | U4a | U4b => set([Row::Ready, Row::D12, Row::D13]),
        _ => set([Row::Ready]),
    }
}

fn family(kp: KillPoint) -> &'static str {
    let n = format!("{kp:?}");
    if n.starts_with('K') {
        "K"
    } else {
        "U"
    }
}

#[test]
fn every_kill_point_every_operation_every_crash_image() {
    let mut hit: BTreeSet<KillPoint> = BTreeSet::new();
    let (mut kills, mut images, mut unique) = (0, 0, 0);
    let mut by_fault: BTreeMap<char, usize> = BTreeMap::new();
    let mut rows: BTreeMap<(&str, Row), usize> = BTreeMap::new();
    let faults = |kp: KillPoint| {
        [Fault::Kill, Fault::Power]
            .into_iter()
            .chain((kp == KillPoint::K15).then_some(Fault::IoCommitted))
            .chain(Some(Fault::Io))
            .collect::<Vec<_>>()
    };
    for s in &SCENARIOS {
        let p = parts(s);
        for kp in KillPoint::ALL {
            for fault in faults(kp) {
                for skip in 0.. {
                    let sc = Scratch::new("kill");
                    let mut store = start(&p, s, &sc, MemDir::new()).into_store();
                    store.arm(kp, fault, skip);
                    let chain = p.chain(store);
                    let r = run(&p, s, &chain);
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
                    assert!(store.log().contains(&kp));
                    hit.insert(kp);
                    kills += 1;
                    let dead = take(store);
                    let mut seen = BTreeSet::new();
                    for (f, pick) in dead.picks(seeds()) {
                        images += 1;
                        let (isc, img) = dead.image(pick);
                        if !seen.insert(img.fingerprint()) {
                            continue;
                        }
                        unique += 1;
                        *by_fault.entry(f).or_default() += 1;
                        let what = format!("{} {kp:?} {fault:?} {pick:?} skip {skip}", s.name);
                        let (row, res) = restart(&p, &isc, img.clone());
                        let (chain, resumed) = res
                            .unwrap_or_else(|e| panic!("{what}: restart refused: {e} ({row:?})"));
                        assert!(allowed(kp).contains(&row), "{what}: row {row:?}");
                        *rows.entry((family(kp), row)).or_default() += 1;
                        assert!(matches!(resumed, Resumed::Head { .. }), "{what}");
                        assert_eq!(chain.head(), dead.published, "{what}: head after restart");
                        assert_eq!(
                            chain.with_store(|s| s.generation()),
                            dead.generation,
                            "{what}: generation after restart"
                        );
                        let store = assert_sound(chain, &p.t);
                        let chain = chain_of(&p, store);
                        finish(s, &chain, &p.t);
                        let store = chain.into_store();
                        assert_eq!(
                            store.flat_root(),
                            p.t.blocks[s.final_head].block.header.state_root,
                            "{what}: state root"
                        );
                        // everything the restart published was synced: the durable bytes alone give it back
                        let head = store.head();
                        let (path, dir) = store.into_parts();
                        let again = MdbxStore::open(
                            &path,
                            dir.crash(Pick::Nothing),
                            xone_fault::cfg(p.retention, p.bpf),
                        )
                        .unwrap();
                        let (c, _) = xone_sync::chain::Chain::open(
                            p.spec.clone(),
                            again,
                            p.exec.clone(),
                            accept as Accept,
                        )
                        .unwrap_or_else(|e| panic!("{what}: second restart: {e}"));
                        assert_eq!(c.head(), head, "{what}: head after the second restart");
                    }
                }
            }
        }
    }
    let missed: Vec<_> = KillPoint::ALL.iter().filter(|k| !hit.contains(k)).collect();
    assert!(missed.is_empty(), "kill points never reached: {missed:?}");
    println!(
        "kill matrix: {} points x {} operations, {kills} kills, {images} crash images ({unique} distinct)",
        hit.len(),
        SCENARIOS.len()
    );
    println!("  distinct images by fault (1 kill-9, 2 power loss, 3 torn, 4 reordered, 5 delayed): {by_fault:?}");
    println!("  reconcile rows after restart: {rows:?}");
}

#[test]
fn a_kill_at_the_commit_loses_nothing_that_was_published() {
    // K15 with an unknown outcome: the manifest decides, and the head is either side of the commit.
    let s = &SCENARIOS[0];
    let p = parts(s);
    let sc = Scratch::new("k15");
    let mut store = start(&p, s, &sc, MemDir::new()).into_store();
    let before = store.head();
    store.arm(KillPoint::K15, Fault::IoCommitted, 0);
    let chain = p.chain(store);
    assert!(chain.insert(p.t.block("m4"), 0).is_err());
    let after = chain.head();
    assert_ne!(before, after, "the commit was applied before the error");
    let dead = take(chain.into_store());
    let (isc, img) = dead.image(Pick::Nothing);
    let (_, res) = restart(&p, &isc, img);
    assert_eq!(res.unwrap().0.head(), after);
}

/// Stub signer: releases a signature only when the store says it may sign.
fn sign(store: &MStore, released: &mut usize) {
    if store.durability().may_sign() {
        *released += 1;
    }
}

fn dead_of(
    path: std::path::PathBuf,
    dir: MemDir,
    published: (u64, alloy_primitives::B256),
) -> Dead {
    Dead {
        path,
        dir,
        published,
        generation: 0,
    }
}

/// F6 at every fsync of the operation: EIO and ENOSPC, at the n-th call. Returns the cases run.
fn failed_fsync(s: &Scn, signed: &mut usize) -> usize {
    let p = parts(s);
    let mut cases = 0;
    for errno in [Errno::Eio, Errno::Enospc] {
        for nth in 1.. {
            let sc = Scratch::new("f6");
            let dir = MemDir::new();
            let chain = start(&p, s, &sc, dir.clone());
            dir.fail_sync(nth, errno);
            let r = run(&p, s, &chain);
            let store = chain.into_store();
            if r.is_ok() {
                break; // the n-th sync does not exist: every fsync of the operation was covered
            }
            cases += 1;
            let what = format!("{} {errno:?} at sync {nth}", s.name);
            assert_eq!(
                store.durability(),
                Durability::Degraded(Cause::FsyncFailed),
                "{what}"
            );
            sign(&store, signed);
            assert!(dir.exists("FSYNC_FAILED"), "{what}: no marker written");
            // no retry in process: a second write touches no file
            let syncs = dir.syncs();
            let chain = p.chain(store);
            assert!(
                chain.set_head(1).is_err(),
                "{what}: a dead store took a write"
            );
            assert_eq!(dir.syncs(), syncs, "{what}: fsync was retried");
            let store = chain.into_store();
            let published = store.head();
            let (path, live) = store.into_parts();
            // the next start in the same boot refuses
            let again = MdbxStore::open(&path, live.clone(), xone_fault::cfg(p.retention, p.bpf));
            assert!(
                matches!(again, Err(OpenError::FsyncFailedSameBoot(_))),
                "{what}: same boot opened"
            );
            // after a reboot (page cache gone, boot id changed) the datadir recovers: any state of the
            // unsynced bytes is a valid image, and MDBX committed what it committed
            let dead = dead_of(path, live, published);
            for (_, pick) in dead.picks(4) {
                let (isc, img) = dead.image(pick);
                img.set_boot("boot-2");
                let (row, res) = restart(&p, &isc, img);
                let (chain, _) = res.unwrap_or_else(|e| panic!("{what} {pick:?}: {e} ({row:?})"));
                assert_eq!(chain.head(), published, "{what} {pick:?}");
                let store = assert_sound(chain, &p.t);
                finish(s, &chain_of(&p, store), &p.t);
            }
        }
    }
    cases
}

#[test]
fn a_failed_fsync_is_fatal_and_the_same_boot_refuses() {
    let mut signed = 0;
    let cases: usize = SCENARIOS.iter().map(|s| failed_fsync(s, &mut signed)).sum();
    assert_eq!(signed, 0, "signatures released while degraded");
    println!("fsync failures: {cases} cases (EIO and ENOSPC at every fsync of 7 operations), 0 signatures released");
}

/// F7 at every write of the operation, once and from there on. Returns (recoveries, fail-stops).
fn no_space(s: &Scn, signed: &mut usize) -> (usize, usize) {
    let p = parts(s);
    let (mut recovered, mut fail_stop) = (0, 0);
    for sticky in [false, true] {
        for nth in 1.. {
            let sc = Scratch::new("f7");
            let dir = MemDir::new();
            let chain = start(&p, s, &sc, dir.clone());
            let before = chain.head();
            dir.fail_write(nth, Errno::Enospc, sticky);
            let r = run(&p, s, &chain);
            let store = chain.into_store();
            if r.is_ok() {
                break;
            }
            let what = format!("{} sticky={sticky} write {nth}", s.name);
            sign(&store, signed);
            if store.durability() != Durability::Degraded(Cause::NoSpace) {
                // cleanup failed, or the failing write came after the commit returned: fail-stop
                fail_stop += 1;
                assert!(
                    matches!(
                        store.durability(),
                        Durability::Degraded(Cause::Io | Cause::FsyncFailed)
                    ),
                    "{what}: {:?}",
                    store.durability()
                );
                continue;
            }
            recovered += 1;
            assert!(store.head().0 <= before.0, "{what}: the head moved forward");
            // reads still work; the tail is cut to the committed length; no orphan chunk
            assert!(
                store.canonical_block(store.head().0).unwrap().is_some(),
                "{what}"
            );
            let m = store.manifest().unwrap().clone();
            for sg in xone_db::Segment::ALL {
                let e = xone_db::segments::entry(&m, sg);
                assert_eq!(
                    store.segs().chunks(sg).unwrap().last().copied(),
                    Some(xone_db::segments::chunk_of(e.last_block, 4)),
                    "{what}: an orphan chunk file is left"
                );
            }
            // space is back: the operation resumes and the store may sign again
            dir.clear_faults();
            let chain = p.chain(store);
            finish(s, &chain, &p.t);
            assert_eq!(
                chain.with_store(|s| s.durability()),
                Durability::Durable,
                "{what}"
            );
        }
    }
    (recovered, fail_stop)
}

#[test]
fn no_space_degrades_cleans_up_and_resumes() {
    let mut signed = 0;
    let (mut r, mut f) = (0, 0);
    for s in &SCENARIOS {
        let (a, b) = no_space(s, &mut signed);
        (r, f) = (r + a, f + b);
    }
    assert_eq!(signed, 0);
    assert!(r > 0 && f > 0);
    println!("ENOSPC: {r} recoveries, {f} fail-stops (cleanup failed, or after the commit), 0 signatures released");
}

/// F8: EIO on a write is fail-stop; a read error at start-up is a typed refusal or a correct head.
fn eio(s: &Scn) -> usize {
    let p = parts(s);
    let mut cases = 0;
    for nth in 1.. {
        let sc = Scratch::new("f8w");
        let dir = MemDir::new();
        let chain = start(&p, s, &sc, dir.clone());
        dir.fail_write(nth, Errno::Eio, false);
        let r = run(&p, s, &chain);
        let store = chain.into_store();
        if r.is_ok() {
            break;
        }
        cases += 1;
        assert!(
            matches!(
                store.durability(),
                Durability::Degraded(Cause::Io | Cause::FsyncFailed)
            ),
            "{} EIO at write {nth}: {:?}",
            s.name,
            store.durability()
        );
        assert!(dir.exists("FSYNC_FAILED"));
    }
    let sc = Scratch::new("f8r");
    let dead = take_any(start(&p, s, &sc, MemDir::new()).into_store());
    for nth in 1..40 {
        let (isc, img) = dead.image(Pick::Nothing);
        img.fail_read(nth);
        let (_, res) = restart(&p, &isc, img.clone());
        if let Ok((chain, _)) = res {
            img.clear_faults();
            assert_eq!(
                chain.head(),
                dead.published,
                "read {nth} failed, yet the head moved"
            );
        }
        cases += 1;
    }
    cases
}

#[test]
fn eio_is_fail_stop_and_a_read_error_at_start_is_a_typed_refusal() {
    let cases: usize = SCENARIOS.iter().map(eio).sum();
    println!("EIO: {cases} cases, never a panic, never a silent wrong head");
}
