//! Crash images and the oracle every restart is held to (tests/fault/PLAN.md section 2).
use std::collections::BTreeSet;

use xone_db::{
    fault::{MemDir, Pick},
    reconcile, Durability, ManifestState, Recovery, Segment,
};
use xone_fault::{cfg, copy_dir, Scratch};
use xone_sync::{
    chain::{Chain, ResumeError, Resumed},
    mdbx::MdbxStore,
    store::Store,
};

use super::*;

/// What `reconcile` said on a restart, in the ADR-005 table's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    /// D11
    Ready,
    /// D12: a torn tail, no complete extra frame.
    D12,
    /// D13: 1 to 128 complete extra frames.
    D13,
    /// D14: more, quarantined.
    D14,
    Refuse,
    Fresh,
}

pub fn classify(r: &Recovery) -> Row {
    match r {
        Recovery::Fresh => Row::Fresh,
        Recovery::Ready => Row::Ready,
        Recovery::Refuse(_) => Row::Refuse,
        Recovery::Repair(a) => {
            let most = a.iter().map(|x| x.dropped_frames).max().unwrap_or(0);
            match (most, a.iter().any(|x| x.quarantine)) {
                (_, true) => Row::D14,
                (0, _) => Row::D12,
                _ => Row::D13,
            }
        }
    }
}

/// A store that died: the MDBX directory (closed) and the disk with its unsynced operations.
pub struct Dead {
    pub path: std::path::PathBuf,
    pub dir: MemDir,
    pub published: (u64, alloy_primitives::B256),
    pub generation: u64,
}

pub fn take(store: MStore) -> Dead {
    assert!(!store.durability().may_sign(), "a dead store may not sign");
    take_any(store)
}

/// The disk of a store that stopped cleanly or died.
pub fn take_any(store: MStore) -> Dead {
    let (published, generation) = (store.head(), store.generation());
    let (path, dir) = store.into_parts();
    Dead {
        path,
        dir,
        published,
        generation,
    }
}

impl Dead {
    /// The picks of one crash: exhaustive first, then seeded.
    pub fn picks(&self, seeds: u64) -> Vec<(char, Pick)> {
        let n = self.dir.pending();
        let mut v = vec![('1', Pick::Everything), ('2', Pick::Nothing)];
        for i in 0..n {
            v.push(('4', Pick::Only(i)));
            v.push(('4', Pick::Except(i)));
            v.push(('5', Pick::Prefix(i)));
            for grid in [512, 64, 1] {
                v.push((
                    '3',
                    Pick::Torn {
                        keep: i,
                        grid,
                        seed: i as u64 + 1,
                    },
                ));
            }
        }
        v.extend((0..seeds).map(|seed| ('4', Pick::Random { seed })));
        v
    }

    /// The restart on one image: a copy of the MDBX directory and the disk `pick` leaves.
    pub fn image(&self, pick: Pick) -> (Scratch, MemDir) {
        let s = Scratch::new("img");
        copy_dir(&self.path, &s.path().join("chain.mdbx"));
        (s, self.dir.crash(pick))
    }
}

/// Opens an image and runs `Chain::open`; returns the row `reconcile` chose.
pub fn restart(
    p: &Parts,
    scratch: &Scratch,
    image: MemDir,
) -> (Row, Result<(MChain, Resumed), ResumeError>) {
    let store = MdbxStore::open(
        &scratch.path().join("chain.mdbx"),
        image,
        cfg(p.retention, p.bpf),
    )
    .unwrap_or_else(|e| panic!("open: {e}"));
    let scan = store.scan();
    let row = classify(&reconcile(scan.generation, &scan.manifest, &scan.tails));
    (
        row,
        Chain::open(p.spec.clone(), store, p.exec.clone(), accept as Accept),
    )
}

/// The checks of PLAN.md section 2 on a restarted datadir: every block up to the head reads back
/// with a valid CRC, nothing beyond the manifest is served, and a second scan finds nothing to do.
pub fn assert_sound(chain: MChain, t: &Tree) -> MStore {
    let label = consistent(&chain, t);
    let _ = label;
    let store = chain.into_store();
    let m = store.manifest().expect("a manifest").clone();
    for n in m.entries[0].first_block..=m.head_number {
        assert!(
            matches!(store.canonical_block(n), Ok(Some(_))),
            "block {n} does not read back"
        );
    }
    for s in Segment::ALL {
        let e = xone_db::segments::entry(&m, s);
        assert!(
            store.segs().read(s, m.head_number + 1, e.len).is_err(),
            "{s:?}: a frame beyond the manifest length was served"
        );
    }
    let scan = store.scan();
    assert_eq!(
        classify(&reconcile(scan.generation, &scan.manifest, &scan.tails)),
        Row::Ready,
        "the repair left something to repair"
    );
    assert!(matches!(scan.manifest, ManifestState::Ok(_)));
    assert_eq!(store.durability(), Durability::Durable);
    store
}

pub fn set<T: Ord>(v: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    v.into_iter().collect()
}

pub fn chain_of(p: &Parts, store: MStore) -> MChain {
    Chain::new(p.spec.clone(), store, p.exec.clone(), accept as Accept)
}
