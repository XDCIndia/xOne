#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! Import, set-head, floor and restart behaviours on synthetic trees with real state roots.

mod common;
use alloy_primitives::B256;
use common::*;
use xone_db::Refuse;
use xone_sync::{
    chain::{
        set_head::SetHeadError, side_index::SIDECHAIN_CHILD_CAP, Chain, ChainRead, InsertError,
        Outcome, ReorgError, ResumeError, Resumed,
    },
    testkit::{Cut, MemStore},
};

fn linear(v2: bool, n: u64) -> (TestChain, Tree) {
    let mut w = World::new(v2, 0);
    for i in 1..=n {
        w.make(&format!("m{i}"));
    }
    let store = w.genesis_store(64, 4);
    w.into_chain(store)
}

#[test]
fn set_head_then_reimport_gives_identical_roots() {
    for v2 in [true, false] {
        let (chain, t) = linear(v2, 12);
        let mut roots = vec![];
        for i in 1..=12 {
            chain.insert(t.block(&format!("m{i}")), 0).unwrap();
            roots.push(chain.head());
        }
        let before = chain.committed();
        // V2 refuses to go below the committed block (block 9 here); V1 has none.
        let target = if v2 { 9 } else { 5 };
        chain.set_head(target).unwrap();
        assert_eq!(chain.head(), roots[target as usize - 1]);
        t.assert_state(&chain, &format!("m{target}"));
        assert_eq!(chain.reader().canonical_hash(target + 1), None);
        assert!(
            chain
                .reader()
                .header(t.hash(&format!("m{}", target + 1)))
                .is_none(),
            "set-head deletes the blocks above"
        );
        // Re-import: every block is executed again and lands on the same head and the same state.
        for i in target + 1..=12 {
            assert_eq!(
                chain.insert(t.block(&format!("m{i}")), 0),
                Ok(Outcome::Extended)
            );
            assert_eq!(chain.head(), roots[i as usize - 1]);
            t.assert_state(&chain, &format!("m{i}"));
        }
        assert_eq!(
            chain.committed(),
            before,
            "v2 {v2}: the floor is re-derived to the same block"
        );
        let store = chain.into_store();
        assert_eq!(store.flat_root(), t.blocks["m12"].block.header.state_root);
    }
}

#[test]
fn set_head_refusals_and_noop() {
    let (chain, t) = linear(true, 10);
    for i in 1..=10 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    let floor = chain.committed().unwrap().number; // 7
    assert_eq!(
        chain.set_head(11),
        Err(SetHeadError::AboveHead {
            target: 11,
            head: 10
        })
    );
    assert_eq!(
        chain.set_head(floor - 1),
        Err(SetHeadError::BelowCommitted {
            target: floor - 1,
            committed: floor
        })
    );
    assert_eq!(chain.head().0, 10);
    chain.set_head(10).unwrap();
    chain.set_head(floor).unwrap();
    assert_eq!(chain.head().1, t.hash(&format!("m{floor}")));
}

/// geth `SetHead` tells the engine to drop its caches (`blockchain.go:1198-1203`).
#[test]
fn set_head_invalidates_the_consensus_caches() {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    struct Count(Arc<AtomicU64>);
    impl xone_sync::chain::HeaderCheck for Count {
        fn check(
            &self,
            _: &dyn ChainRead,
            _: &xone_primitives::Header,
            _: &xone_primitives::Header,
            _: u64,
        ) -> Result<(), xone_consensus::verify::HeaderError> {
            Ok(())
        }
        fn invalidate(&self, n: u64) {
            self.0.store(n, Ordering::SeqCst);
        }
    }
    let seen = Arc::new(AtomicU64::new(u64::MAX));
    let mut w = World::new(false, 0);
    w.make("m4");
    let store = w.genesis_store(64, 4);
    let chain = Chain::new(w.spec, store, w.exec, Count(seen.clone()));
    for i in 1..=4 {
        chain
            .insert(w.blocks[&format!("m{i}")].block.clone(), 0)
            .unwrap();
    }
    chain.set_head(4).unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        u64::MAX,
        "a no-op rewind drops nothing"
    );
    chain.set_head(2).unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn set_head_below_retained_state_is_refused() {
    let mut w = World::new(false, 0);
    w.make("m12");
    let store = w.genesis_store(4, 4); // four blocks of state
    let (chain, t) = w.into_chain(store);
    for i in 1..=12 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    assert_eq!(chain.set_head(2), Err(SetHeadError::NotRetained(2)));
    assert_eq!(chain.head().0, 12);
    chain.set_head(9).unwrap();
}

/// The malicious deep reorg: an alternative branch from far below the committed block that grows
/// one block past the head. It is refused with the typed error, head, index and state do not move,
/// also after a restart (the floor is persisted).
#[test]
fn deep_reorg_below_the_committed_block_is_refused() {
    let mut w = World::new(true, 1);
    for i in 2..=9 {
        w.make(&format!("m{i}"));
    }
    for i in 2..=10 {
        w.make(&format!("a{i}"));
    }
    let store = w.genesis_store(64, 4);
    let (chain, t) = w.into_chain(store);
    chain.insert(t.block("s1"), 0).unwrap();
    for i in 2..=9 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    let floor = chain.committed().unwrap();
    assert_eq!((floor.number, floor.hash), (6, t.hash("m6")));
    for i in 2..=9 {
        assert_eq!(
            chain.insert(t.block(&format!("a{i}")), 0),
            Ok(Outcome::Side),
            "a{i}"
        );
    }
    let r = chain.insert(t.block("a10"), 0);
    assert_eq!(
        r,
        Err(InsertError::Reorg(ReorgError::BelowCommitted {
            common: 1,
            committed: 6,
            hash: t.hash("m6")
        }))
    );
    assert_eq!(chain.head(), (9, t.hash("m9")));
    t.assert_state(&chain, "m9");
    for n in 1..=9 {
        assert!(chain.reader().canonical_hash(n).is_some());
    }
    // Restart: the floor is in the store, the same attack is refused again.
    let disk = chain.into_store().crash(Cut::Kill9);
    let mut w2 = World::new(true, 1);
    for i in 2..=10 {
        w2.make(&format!("a{i}"));
    }
    for i in 2..=9 {
        w2.make(&format!("m{i}"));
    }
    let (chain, resumed) = Chain::open(
        w2.spec.clone(),
        MemStore::open(disk),
        w2.exec,
        accept as Accept,
    )
    .unwrap();
    assert!(matches!(resumed, Resumed::Head { number: 9, .. }));
    assert_eq!(chain.committed().unwrap().number, 6);
    let r = chain.insert(w2.blocks["a10"].block.clone(), 0);
    assert!(
        matches!(
            r,
            Err(InsertError::Reorg(ReorgError::BelowCommitted { .. }))
        ),
        "{r:?}"
    );
}

#[test]
fn reorg_above_the_committed_block_goes_through() {
    let mut w = World::new(true, 6);
    for i in 7..=9 {
        w.make(&format!("m{i}"));
    }
    for i in 7..=10 {
        w.make(&format!("a{i}"));
    }
    let store = w.genesis_store(64, 4);
    let (chain, t) = w.into_chain(store);
    for i in 1..=6 {
        chain.insert(t.block(&format!("s{i}")), 0).unwrap();
    }
    for i in 7..=9 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    assert_eq!(chain.committed().unwrap().number, 6);
    for i in 7..=9 {
        chain.insert(t.block(&format!("a{i}")), 0).unwrap();
    }
    let r = chain.insert(t.block("a10"), 0);
    assert_eq!(
        r,
        Ok(Outcome::Adopted {
            dropped: 3,
            added: 4
        })
    );
    assert_eq!(chain.head().1, t.hash("a10"));
    t.assert_state(&chain, "a10");
    // the old branch is kept as side blocks and can be adopted again
    assert!(chain.reader().header(t.hash("m9")).is_some());
}

#[test]
fn the_floor_never_moves_down() {
    let (chain, t) = linear(true, 9);
    let mut last = 0;
    for i in 1..=9 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
        let f = chain.committed().map_or(0, |l| l.number);
        assert!(f >= last);
        last = f;
    }
    assert_eq!(last, 6);
}

fn rejects(chain: &TestChain, b: xone_sync::store::Block) -> InsertError {
    let before = chain.head();
    let e = chain.insert(b, 0).unwrap_err();
    assert_eq!(chain.head(), before);
    e
}

#[test]
fn every_check_runs_on_every_import() {
    use xone_consensus::verify::BodyError;
    // The same corruption at the first block and in the middle of a run: no position is exempt.
    for (name, edit) in [
        ("tx root", 0),
        ("receipt root", 1),
        ("bloom", 2),
        ("gas used", 3),
        ("state root", 4),
        ("uncle hash", 5),
    ] {
        for v2 in [true, false] {
            for at in [1u64, 6] {
                let mut w = World::new(v2, 0);
                for i in 1..at {
                    w.make(&format!("m{i}"));
                }
                let bad = w.variant(&format!("m{at}"), |h| match edit {
                    0 => h.tx_root = B256::repeat_byte(7),
                    1 => h.receipt_root = B256::repeat_byte(7),
                    2 => h.bloom.0[3] = 1,
                    3 => h.gas_used = 9,
                    4 => h.state_root = B256::repeat_byte(7),
                    _ => h.uncle_hash = B256::repeat_byte(7),
                });
                let store = w.genesis_store(64, 4);
                let (chain, t) = w.into_chain(store);
                for i in 1..at {
                    chain.insert(t.block(&format!("m{i}")), 0).unwrap();
                }
                let e = rejects(&chain, bad);
                let ok = match edit {
                    0 => matches!(e, InsertError::Body(BodyError::TxRootMismatch { .. })),
                    1 => matches!(e, InsertError::Body(BodyError::ReceiptRootMismatch { .. })),
                    2 => matches!(e, InsertError::Body(BodyError::BloomMismatch { .. })),
                    3 => matches!(e, InsertError::GasUsed { .. }),
                    4 => matches!(e, InsertError::StateRoot { .. }),
                    _ => matches!(e, InsertError::Body(BodyError::UncleHashMismatch { .. })),
                };
                assert!(ok, "{name} v2 {v2} at {at}: {e:?}");
            }
        }
    }
}

#[test]
fn header_check_runs_before_anything_is_stored() {
    use xone_consensus::verify::HeaderError;
    let mut w = World::new(true, 0);
    let b = w.block("m1");
    let store = w.genesis_store(64, 4);
    let reject: Accept = |_, _, _, _| Err(HeaderError::InvalidMixDigest);
    let chain = Chain::new(w.spec, store, w.exec, reject);
    assert_eq!(
        chain.insert(b.clone(), 0),
        Err(InsertError::Header(HeaderError::InvalidMixDigest))
    );
    assert!(chain.reader().header(b.hash()).is_none());
    // the real verifier is what a node plugs in; its refusal of the genesis rule is typed too
    let real =
        |_: &dyn ChainRead, h: &xone_primitives::Header, p: &xone_primitives::Header, now: u64| {
            let spec = spec(true);
            xone_consensus::verify::HeaderVerifier::new(&spec).verify(
                h,
                p,
                now,
                &xone_consensus::verify::Context::V2(Default::default()),
            )
        };
    let mut w = World::new(true, 0);
    let b = w.block("m1");
    let store = w.genesis_store(64, 4);
    let chain = Chain::new(w.spec, store, w.exec, real);
    let e = chain.insert(b, 0).unwrap_err();
    assert!(matches!(e, InsertError::Header(_)), "{e:?}");
    assert_eq!(chain.head().0, 0);
}

#[test]
fn unknown_parent_known_block_and_side_blocks() {
    let mut w = World::new(true, 1);
    for l in ["m2", "m3", "a2", "a3"] {
        w.make(l);
    }
    let store = w.genesis_store(64, 4);
    let (chain, t) = w.into_chain(store);
    assert!(matches!(
        chain.insert(t.block("m2"), 0),
        Err(InsertError::UnknownParent { number: 2, .. })
    ));
    chain.insert(t.block("s1"), 0).unwrap();
    chain.insert(t.block("m2"), 0).unwrap();
    assert_eq!(chain.insert(t.block("m2"), 0), Ok(Outcome::Known));
    assert_eq!(chain.insert(t.block("s1"), 0), Ok(Outcome::Known));
    assert_eq!(chain.insert(t.block("a2"), 0), Ok(Outcome::Side));
    // a known side block is not executed again and does not move the head (geth `writeKnownBlock`)
    assert_eq!(chain.insert(t.block("a2"), 0), Ok(Outcome::Side));
    assert_eq!(chain.head().1, t.hash("m2"));
    assert_eq!(
        chain.insert(t.block("a3"), 0),
        Ok(Outcome::Adopted {
            dropped: 1,
            added: 2
        })
    );
    // a side block whose state is shared with the head is still executed on its own parent
    t.assert_state(&chain, "a3");
}

/// geth `insertSideChain`: a block whose parent state is gone is stored without state, at most
/// `SIDECHAIN_CHILD_CAP` per parent, and is never adopted.
#[test]
fn stateless_side_blocks_are_capped() {
    let mut w = World::new(true, 1);
    for i in 2..=20 {
        w.make(&format!("m{i}"));
    }
    // siblings of m5 whose parent (m4) will be out of the state window
    let kids: Vec<_> = (0..SIDECHAIN_CHILD_CAP as u64 + 2)
        .map(|k| {
            w.variant("m5", move |h| {
                h.timestamp += 100 + k;
                h.state_root = alloy_primitives::keccak256(k.to_le_bytes());
            })
        })
        .collect();
    let store = w.genesis_store(3, 4);
    let (chain, t) = w.into_chain(store);
    chain.insert(t.block("s1"), 0).unwrap();
    for i in 2..=20 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    for (i, k) in kids.iter().enumerate() {
        let r = chain.insert(k.clone(), 0);
        if i < SIDECHAIN_CHILD_CAP {
            assert_eq!(r, Ok(Outcome::Stateless), "child {i}");
        } else {
            assert!(matches!(r, Err(InsertError::Side(_))), "child {i}: {r:?}");
        }
    }
    assert_eq!(chain.head().0, 20);
    // the canonical block re-imported with pruned state is a no-op
    assert_eq!(chain.insert(t.block("m5"), 0), Ok(Outcome::Known));
    // a stateless block with the root of the canonical block at its height is refused
    let ghost = {
        let mut h = t.block("m5").header;
        h.timestamp += 1000;
        xone_sync::store::Block::new(h, Default::default())
    };
    let r = chain.insert(ghost, 0);
    assert!(matches!(r, Err(InsertError::Side(_))), "{r:?}");
}

// ---- restart ----

#[test]
fn resume_resumes_the_head_and_never_starts_at_zero() {
    let (chain, t) = linear(true, 10);
    for i in 1..=10 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    let disk = chain.into_store().crash(Cut::PowerLoss);
    let w = World::new(true, 0);
    let (chain, r) = Chain::open(w.spec, MemStore::open(disk), w.exec, accept as Accept).unwrap();
    assert!(matches!(
        r,
        Resumed::Head {
            number: 10,
            repaired: 0,
            ..
        }
    ));
    assert_eq!(chain.head(), (10, t.hash("m10")));
    t.assert_state(&chain, "m10");
}

fn corrupted(f: impl FnOnce(&mut xone_sync::testkit::Disk, &Tree)) -> ResumeError {
    let (chain, t) = linear(true, 10);
    for i in 1..=10 {
        chain.insert(t.block(&format!("m{i}")), 0).unwrap();
    }
    let mut disk = chain.into_store().crash(Cut::Kill9);
    f(&mut disk, &t);
    let w = World::new(true, 0);
    Chain::open(w.spec, MemStore::open(disk), w.exec, accept as Accept)
        .err()
        .expect("a damaged datadir must not open")
}

#[test]
fn a_corrupted_datadir_is_refused_naming_the_missing_record() {
    // the xOneGo#720 shape: the manifest says #10, the canonical index has no hash for it
    assert_eq!(
        corrupted(|d, _| d.damage_canonical(10)),
        ResumeError::MissingCanonical { number: 10 }
    );
    assert_eq!(
        corrupted(|d, _| d.damage_canonical(7)),
        ResumeError::MissingCanonical { number: 7 }
    );
    let e = corrupted(|d, t| d.damage_record(t.hash("m10")));
    assert_eq!(
        e,
        ResumeError::MissingHeader {
            number: 10,
            hash: corrupted_hash(10)
        }
    );
    assert_eq!(
        corrupted(|d, _| d.damage_manifest()),
        ResumeError::Refused(Refuse::ManifestMissing)
    );
    assert!(matches!(
        corrupted(|d, _| d.flip_manifest_byte(30)),
        ResumeError::Refused(Refuse::ManifestCorrupt)
    ));
    assert!(matches!(
        corrupted(|d, _| d.damage_segment_file(0)),
        ResumeError::Refused(Refuse::SegmentMissing(_))
    ));
}

fn corrupted_hash(n: u64) -> B256 {
    let mut w = World::new(true, 0);
    w.hash(&format!("m{n}"))
}

#[test]
fn an_empty_manifest_with_records_is_not_a_new_datadir() {
    // generation 0 and no manifest is `Fresh`; records without a manifest are not.
    let w = World::new(true, 0);
    let e = Chain::open(w.spec, MemStore::empty(4), w.exec, accept as Accept).map(|(_, r)| r);
    assert_eq!(e.unwrap(), Resumed::Fresh);
    let (chain, _) = linear(true, 2);
    let mut disk = chain.into_store().crash(Cut::Kill9);
    disk.damage_manifest();
    let w = World::new(true, 0);
    let e = Chain::open(w.spec, MemStore::open(disk), w.exec, accept as Accept)
        .err()
        .unwrap();
    assert_eq!(e, ResumeError::Refused(Refuse::ManifestMissing));
}

/// The vectors see the planted bugs. Each mutant store runs the scenario of the bug it stands for
/// and the scenario fails where the real store passes.
#[test]
fn planted_bugs_are_caught() {
    // CertiK XDC-08: with the floor forgotten the deep reorg of the malicious vector goes through.
    let mut w = World::new(true, 1);
    for i in 2..=9 {
        w.make(&format!("m{i}"));
    }
    for i in 2..=10 {
        w.make(&format!("a{i}"));
    }
    let store = Mutant::new(w.genesis_store(64, 4), Mode::ForgetFloor);
    let blocks = w.blocks;
    let chain = Chain::new(w.spec, store, w.exec, accept as Accept);
    let get = |l: &str| blocks[l].block.clone();
    chain.insert(get("s1"), 0).unwrap();
    for i in 2..=9 {
        chain.insert(get(&format!("m{i}")), 0).unwrap();
    }
    for i in 2..=10 {
        let r = chain.insert(get(&format!("a{i}")), 0);
        assert!(r.is_ok(), "a{i}: {r:?}");
    }
    assert_eq!(
        chain.head().1,
        blocks["a10"].block.hash(),
        "the committed block 6 was dropped"
    );

    // d4cdf136: a reorg that does not move the index cannot adopt the sibling's branch.
    let mut w = World::new(true, 3);
    for l in ["m4", "m5", "a4"] {
        w.make(l);
    }
    let store = Mutant::new(w.genesis_store(64, 4), Mode::SkipUnwind);
    let blocks = w.blocks;
    let chain = Chain::new(w.spec, store, w.exec, accept as Accept);
    for l in ["s1", "s2", "s3", "a4", "m4"] {
        chain.insert(blocks[l].block.clone(), 0).unwrap();
    }
    let r = chain.insert(blocks["m5"].block.clone(), 0);
    assert!(matches!(r, Err(InsertError::Store(_))), "{r:?}");
}
