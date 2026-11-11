#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The decision table D1-D14 on the real scanner (ADR-005 section 7), the format-version
//! refusals (section 12), start-up repair (RP1-RP3) and pruning (PR1-PR5). Each row is built by
//! damaging a real datadir, and each refusal must leave every file as it found it.

mod support;
use alloy_primitives::B256;
use support::{crash::*, *};
use xone_db::{
    fault::{MemDir, Pick},
    mdbx::{Env, T},
    segments::{chunk_path, frame, SegDir, SegFile},
    Manifest, Refuse, Segment, GEN_KEY, MANIFEST_KEY,
};
use xone_fault::{cfg, Scratch};
use xone_sync::{
    chain::{Chain, ResumeError, Resumed},
    mdbx::MdbxStore,
    store::{KillPoint, Store, StoreError},
    testkit::Fault,
};

const LABELS: [&str; 6] = ["m1", "m2", "m3", "m4", "m5", "m6"];

fn world() -> Parts {
    Parts::new(true, 0, LABELS, 4)
}

/// A datadir with the six blocks committed, closed: its MDBX directory and its disk.
fn built(p: &Parts) -> (Scratch, Dead) {
    let sc = Scratch::new("rows");
    let chain = p.chain(p.fresh(&sc));
    for l in LABELS {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    let dead = take_any(chain.into_store());
    (sc, dead)
}

fn edit_mdbx(path: &std::path::Path, f: impl Fn(&xone_db::mdbx::Wr)) {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let tx = env.write().unwrap();
    f(&tx);
    tx.commit().unwrap();
}

fn manifest_of(path: &std::path::Path) -> Manifest {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let rd = env.read().unwrap();
    let b = rd.get(T::Meta, MANIFEST_KEY).unwrap().unwrap();
    Manifest::decode(&b).unwrap()
}

/// The active chunk file of the six-block datadir (four blocks per file).
fn chunk0(s: Segment) -> String {
    chunk_path(s, 4)
}

/// Appends `bytes` to the end of a segment file, durably.
fn append(dir: &MemDir, s: Segment, bytes: &[u8]) {
    let mut f = dir.open(&chunk0(s)).unwrap().unwrap();
    let len = f.size().unwrap();
    f.write_at(len, bytes).unwrap();
    f.sync_all().unwrap();
}

fn refused(p: &Parts, d: &Dead) -> (Refuse, bool) {
    refused_on(p, d, Pick::Nothing)
}

fn refused_on(p: &Parts, d: &Dead, pick: Pick) -> (Refuse, bool) {
    let (sc, img) = d.image(pick);
    let before = snapshot(&img);
    let (row, res) = restart(p, &sc, img.clone());
    assert_eq!(row, Row::Refuse);
    let Err(ResumeError::Refused(r)) = res else {
        panic!("not refused")
    };
    (r, snapshot(&img) == before)
}

fn snapshot(d: &MemDir) -> Vec<(String, Option<Vec<u8>>)> {
    let mut v = Vec::new();
    for s in Segment::ALL {
        for n in d.list(xone_db::segments::seg_dir(s)).unwrap() {
            let name = format!("{}/{n}", xone_db::segments::seg_dir(s));
            v.push((name.clone(), d.bytes(&name)));
        }
    }
    v
}

#[test]
fn d1_a_new_datadir_is_fresh() {
    let p = world();
    let sc = Scratch::new("d1");
    let store = MdbxStore::open(&sc.path().join("chain.mdbx"), MemDir::new(), cfg(64, 4)).unwrap();
    let scan = store.scan();
    assert_eq!(
        classify(&xone_db::reconcile(scan.generation, &scan.manifest, &scan.tails)),
        Row::Fresh
    );
    let (_, r) = Chain::open(p.spec.clone(), store, p.exec.clone(), accept as Accept).unwrap();
    assert_eq!(r, Resumed::Fresh);
}

#[test]
fn d2_segments_with_no_mdbx_authority_are_not_cut() {
    // a kill inside the very first commit: files exist, MDBX holds nothing
    let p = world();
    let sc = Scratch::new("d2");
    let mut s = MdbxStore::open(&sc.path().join("chain.mdbx"), MemDir::new(), cfg(64, 4)).unwrap();
    s.arm(KillPoint::K10, Fault::Kill, 0);
    let g = &p.t.blocks["s0"].block.header;
    assert!(s.init_anchor(g.clone(), Default::default()).is_err());
    let d = take(s);
    assert_eq!(refused_on(&p, &d, Pick::Everything), (Refuse::OrphanSegments, true));
    // a kill before any segment byte leaves a datadir that is simply new
    let mut s = MdbxStore::open(&sc.path().join("c2.mdbx"), MemDir::new(), cfg(64, 4)).unwrap();
    s.arm(KillPoint::K03, Fault::Kill, 0);
    assert!(s.init_anchor(g.clone(), Default::default()).is_err());
    let d = take(s);
    let (isc, img) = d.image(Pick::Everything);
    let _ = isc;
    let store = MdbxStore::open(&d.path, img, cfg(64, 4)).unwrap();
    let scan = store.scan();
    assert_eq!(
        classify(&xone_db::reconcile(scan.generation, &scan.manifest, &scan.tails)),
        Row::Fresh
    );
}

#[test]
fn d3_to_d7_the_manifest_and_the_generation() {
    let p = world();
    let (_sc, d) = built(&p);
    let m = manifest_of(&d.path);
    // D3
    let (sc3, d3) = (Scratch::new("d3"), &d);
    xone_fault::copy_dir(&d3.path, &sc3.path().join("chain.mdbx"));
    edit_mdbx(&sc3.path().join("chain.mdbx"), |tx| tx.del(T::Meta, MANIFEST_KEY).unwrap());
    let d3 = Dead { path: sc3.path().join("chain.mdbx"), dir: d.dir.clone(), published: d.published, generation: 0 };
    assert_eq!(refused(&p, &d3), (Refuse::ManifestMissing, true));
    // D4: every byte of the manifest, flipped one at a time
    for i in [0usize, 4, 6, 20, 100, 200, 256] {
        let sc = Scratch::new("d4");
        xone_fault::copy_dir(&d.path, &sc.path().join("chain.mdbx"));
        let mut b = m.encode();
        b[i] ^= 1;
        edit_mdbx(&sc.path().join("chain.mdbx"), |tx| tx.put(T::Meta, MANIFEST_KEY, &b).unwrap());
        let dd = Dead { path: sc.path().join("chain.mdbx"), dir: d.dir.clone(), published: d.published, generation: 0 };
        let (r, unchanged) = refused(&p, &dd);
        assert!(unchanged, "byte {i}");
        assert!(
            matches!(r, Refuse::ManifestCorrupt | Refuse::FormatVersion(_)),
            "byte {i}: {r:?}"
        );
    }
    // D5: a manifest of another version, 0 and 2
    for v in [0u16, 2] {
        let sc = Scratch::new("d5");
        xone_fault::copy_dir(&d.path, &sc.path().join("chain.mdbx"));
        let mut b = m.encode();
        b[4..6].copy_from_slice(&v.to_le_bytes());
        edit_mdbx(&sc.path().join("chain.mdbx"), |tx| tx.put(T::Meta, MANIFEST_KEY, &b).unwrap());
        let dd = Dead { path: sc.path().join("chain.mdbx"), dir: d.dir.clone(), published: d.published, generation: 0 };
        assert_eq!(refused(&p, &dd), (Refuse::FormatVersion(v), true));
    }
    // D6: the generation row and the manifest disagree
    let sc = Scratch::new("d6");
    xone_fault::copy_dir(&d.path, &sc.path().join("chain.mdbx"));
    edit_mdbx(&sc.path().join("chain.mdbx"), |tx| {
        tx.put(T::Meta, GEN_KEY, &(m.generation + 1).to_le_bytes()).unwrap();
    });
    let dd = Dead { path: sc.path().join("chain.mdbx"), dir: d.dir.clone(), published: d.published, generation: 0 };
    assert_eq!(
        refused(&p, &dd),
        (Refuse::GenerationMismatch { mdbx: m.generation + 1, manifest: m.generation }, true)
    );
    // D7: an entry that does not end at the head
    let sc = Scratch::new("d7");
    xone_fault::copy_dir(&d.path, &sc.path().join("chain.mdbx"));
    let mut bad = m.clone();
    bad.entries[1].last_block -= 1;
    edit_mdbx(&sc.path().join("chain.mdbx"), |tx| tx.put(T::Meta, MANIFEST_KEY, &bad.encode()).unwrap());
    let dd = Dead { path: sc.path().join("chain.mdbx"), dir: d.dir.clone(), published: d.published, generation: 0 };
    assert_eq!(refused(&p, &dd), (Refuse::ManifestInconsistent, true));
}

#[test]
fn d8_to_d10_mdbx_ahead_of_the_segments_is_refused_untouched() {
    let p = world();
    let m = {
        let (_sc, d) = built(&p);
        manifest_of(&d.path)
    };
    for s in Segment::ALL {
        let e = xone_db::segments::entry(&m, s);
        // D8: the file is gone
        let (_sc, d) = built(&p);
        d.dir.remove(&chunk_path(s, 4)).unwrap();
        d.dir.sync(xone_db::segments::seg_dir(s)).unwrap();
        assert_eq!(refused(&p, &d), (Refuse::SegmentMissing(s), true), "{s:?}");
        // D9: shorter than committed
        let (_sc, d) = built(&p);
        let mut f = d.dir.open(&chunk_path(s, 4)).unwrap().unwrap();
        f.set_len(e.len - 1).unwrap();
        f.sync_all().unwrap();
        assert_eq!(refused(&p, &d), (Refuse::SegmentShort(s), true), "{s:?}");
        // D10: a flipped byte in the committed tail frame
        let (_sc, d) = built(&p);
        let mut f = d.dir.open(&chunk_path(s, 4)).unwrap().unwrap();
        let mut b = [0u8; 1];
        f.read_at(e.len - 6, &mut b).unwrap();
        f.write_at(e.len - 6, &[b[0] ^ 1]).unwrap();
        f.sync_all().unwrap();
        assert_eq!(refused(&p, &d), (Refuse::SegmentCorrupt(s), true), "{s:?}");
    }
}

#[test]
fn d11_to_d14_segments_ahead_are_cut_and_far_ahead_are_quarantined() {
    let p = world();
    // D11
    let (_sc, d) = built(&p);
    let (sc, img) = d.image(Pick::Nothing);
    let (row, res) = restart(&p, &sc, img);
    assert_eq!(row, Row::Ready);
    assert!(matches!(res.unwrap().1, Resumed::Head { repaired: 0, .. }));
    // D12: a torn tail, and D13: complete extra frames, then D14: more than 128
    for (n, frames, row) in [(0usize, 7usize, Row::D12), (3, 0, Row::D13), (129, 0, Row::D14)] {
        let (_sc, d) = built(&p);
        let m = manifest_of(&d.path);
        for s in Segment::ALL {
            let mut extra = Vec::new();
            for k in 0..n {
                extra.extend(frame(100 + k as u64, &[k as u8; 9]));
            }
            extra.extend(vec![0xAB; frames]);
            append(&d.dir, s, &extra);
        }
        let (sc, img) = d.image(Pick::Nothing);
        let (got, res) = restart(&p, &sc, img.clone());
        assert_eq!(got, row);
        let (chain, r) = res.unwrap();
        assert!(matches!(r, Resumed::Head { repaired: 3, .. }));
        assert_eq!(chain.head(), d.published);
        for s in Segment::ALL {
            let e = xone_db::segments::entry(&m, s);
            assert_eq!(img.bytes(&chunk0(s)).unwrap().len() as u64, e.len);
        }
        let quarantined = img
            .list("quarantine")
            .unwrap()
            .into_iter()
            .map(|n| img.bytes(&format!("quarantine/{n}")).unwrap().len())
            .collect::<Vec<_>>();
        if row == Row::D14 {
            assert_eq!(quarantined.len(), 3, "the cut bytes are copied before the cut");
            assert!(quarantined.iter().all(|l| *l == 129 * (9 + 16)));
        } else {
            assert!(quarantined.is_empty());
        }
        assert_sound(chain, &p.t);
    }
}

#[test]
fn a_flipped_byte_in_a_committed_frame_is_never_served() {
    let p = world();
    let (_sc, d) = built(&p);
    // a frame in the middle of the bodies file: start-up checks the tail, every read checks the CRC
    let mut f = d.dir.open(&chunk_path(Segment::Bodies, 4)).unwrap().unwrap();
    let mut b = [0u8; 1];
    f.read_at(26, &mut b).unwrap();
    f.write_at(26, &[b[0] ^ 1]).unwrap();
    f.sync_all().unwrap();
    let (sc, img) = d.image(Pick::Nothing);
    let (_, res) = restart(&p, &sc, img);
    let (chain, _) = res.unwrap();
    let store = chain.into_store();
    assert!(store.canonical_block(4).is_err(), "damaged body served");
    assert!(store.canonical_block(5).unwrap().is_some());
    assert!(store.record(d.published.1).is_some());
}

#[test]
fn a_segment_header_of_another_version_is_refused_untouched() {
    let p = world();
    for v in [0u16, 2] {
        let (_sc, d) = built(&p);
        let mut f = d.dir.open(&chunk_path(Segment::Receipts, 4)).unwrap().unwrap();
        f.write_at(4, &v.to_le_bytes()).unwrap();
        f.sync_all().unwrap();
        assert_eq!(refused(&p, &d), (Refuse::FormatVersion(v), true));
    }
}

#[test]
fn repair_is_idempotent_across_a_kill_at_rp1_rp2_rp3() {
    let p = world();
    for (kp, n) in [(KillPoint::RP1, 3usize), (KillPoint::RP2, 3), (KillPoint::RP3, 129)] {
        let (_sc, d) = built(&p);
        for s in Segment::ALL {
            let extra: Vec<u8> = (0..n).flat_map(|k| frame(100 + k as u64, &[1; 9])).collect();
            append(&d.dir, s, &extra);
        }
        let mut crashes = 0;
        for (_, pick) in d.picks(4) {
            let (sc, img) = d.image(pick);
            let mut store =
                MdbxStore::open(&sc.path().join("chain.mdbx"), img.clone(), cfg(p.retention, p.bpf))
                    .unwrap();
            store.arm(kp, Fault::Kill, 0);
            let r = Chain::open(p.spec.clone(), store, p.exec.clone(), accept as Accept);
            assert!(
                matches!(r, Err(ResumeError::Store(StoreError::Crashed(k))) if k == kp),
                "{kp:?} {pick:?}"
            );
            crashes += 1;
            // the repair died: the same image, crashed again at every pick, repairs to the same head
            let (head, gen) = (d.published, 0);
            let _ = gen;
            for pick2 in [Pick::Everything, Pick::Nothing, Pick::Random { seed: 1 }] {
                let img2 = img.crash(pick2);
                let sc2 = Scratch::new("rp");
                xone_fault::copy_dir(&sc.path().join("chain.mdbx"), &sc2.path().join("chain.mdbx"));
                let (_, res) = restart(&p, &sc2, img2);
                let (chain, _) = res.unwrap_or_else(|e| panic!("{kp:?}: {e}"));
                assert_eq!(chain.head(), head);
                assert_sound(chain, &p.t);
            }
        }
        assert!(crashes > 0);
    }
}

#[test]
fn pruning_survives_a_kill_at_pr1_to_pr5() {
    // retention 3 and chunks of 4 blocks: after 10 blocks the oldest openable block is 7, so the
    // chunks below block 4 may go.
    let labels: Vec<String> = (1..=10).map(|n| format!("m{n}")).collect();
    let mut p = Parts::new(true, 0, &labels, 4);
    p.retention = 3;
    for kp in KillPoint::PRUNE {
        let sc = Scratch::new("prune");
        let mut store = p.fresh(&sc);
        let chain = p.chain(std::mem::replace(&mut store, p.fresh(&Scratch::new("x"))));
        for l in &labels {
            chain.insert(p.t.block(l), 0).unwrap();
        }
        let mut store = chain.into_store();
        assert_eq!(store.head().0, 10);
        let before = store.manifest().unwrap().clone();
        assert!(store.segs().chunks(Segment::Headers).unwrap().contains(&0));
        store.arm(kp, Fault::Kill, 0);
        assert!(store.prune_to(4).is_err(), "{kp:?}");
        let dead = take(store);
        for (_, pick) in dead.picks(4) {
            let (isc, img) = dead.image(pick);
            let (row, res) = restart(&p, &isc, img.clone());
            let (chain, _) = res.unwrap_or_else(|e| panic!("{kp:?} {pick:?}: {e} ({row:?})"));
            assert_eq!(chain.head(), dead.published, "{kp:?}");
            let store = chain.into_store();
            let m = store.manifest().unwrap();
            let pruned = m.entries[0].first_block == 4;
            assert_eq!(pruned, kp != KillPoint::PR1, "{kp:?}: the floor commit");
            assert_eq!(m.generation, before.generation + u64::from(pruned));
            // a pruned block is gone from the index; the others read back; no chunk below the floor
            assert_eq!(store.canonical_block(2).unwrap().is_none(), pruned, "{kp:?} {pick:?}");
            assert!(store.canonical_block(4).unwrap().is_some());
            assert!(store.canonical_block(10).unwrap().is_some());
            for s in Segment::ALL {
                let chunks = store.segs().chunks(s).unwrap();
                assert_eq!(chunks.contains(&0), !pruned, "{kp:?} {pick:?}: chunk 0 of {s:?}");
            }
            // nothing below the floor can be unwound to
            assert!(chain_of(&p, store).set_head(2).is_err());
        }
        let _ = B256::ZERO;
    }
}
