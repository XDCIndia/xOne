#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    dead_code
)] // test support: a panic is the failure report
//! The synthetic block trees of xone-sync's tests (real state roots, the geth oracle's scenarios),
//! run over `MdbxStore` on the in-memory disk.

#[path = "../../../../crates/xone-sync/tests/common/mod.rs"]
pub mod common;

pub mod crash;
pub use common::*;
use std::collections::BTreeSet;
use xone_db::fault::{MemDir, Pick};
use xone_fault::{anchored, cfg, Scratch};
use xone_sync::{
    chain::{Chain, ChainRead, ResumeError, Resumed},
    mdbx::MdbxStore,
    store::Store,
};

pub type MStore = MdbxStore<MemDir>;
pub type MChain = Chain<MStore, xone_sync::testkit::VectorExecutor, Accept>;

/// A world built once: every chain of a test gets its own copy of the spec and the recordings.
pub struct Parts {
    pub spec: xone_chainspec::ChainSpec,
    pub exec: xone_sync::testkit::VectorExecutor,
    pub t: Tree,
    pub bpf: u64,
    pub retention: u64,
}

impl Parts {
    pub fn new(
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
            retention: 64,
        }
    }

    pub fn chain(&self, store: MStore) -> MChain {
        Chain::new(
            self.spec.clone(),
            store,
            self.exec.clone(),
            accept as Accept,
        )
    }

    pub fn fresh(&self, scratch: &Scratch) -> MStore {
        self.fresh_on(scratch, MemDir::new())
    }

    /// A new datadir on a disk the test keeps a handle to.
    pub fn fresh_on(&self, scratch: &Scratch, dir: MemDir) -> MStore {
        let g = &self.t.blocks["s0"];
        anchored(
            scratch,
            dir,
            g.block.header.clone(),
            Default::default(),
            cfg(self.retention, self.bpf),
        )
    }

    /// The process start on the disk a crash left: the store is dropped, the segments are cut as
    /// `pick` says, MDBX keeps what committed, and `Chain::open` reconciles.
    pub fn reopen(
        &self,
        scratch: &Scratch,
        store: MStore,
        pick: Pick,
    ) -> Result<(MChain, Resumed), ResumeError> {
        let (path, dir) = store.into_parts();
        let image = dir.crash(pick);
        drop(dir);
        let store = MdbxStore::open(&path, image, cfg(self.retention, self.bpf))
            .unwrap_or_else(|e| panic!("open after crash: {e}"));
        let _ = scratch;
        Chain::open(
            self.spec.clone(),
            store,
            self.exec.clone(),
            accept as Accept,
        )
    }
}

pub fn label_of(t: &Tree, hash: alloy_primitives::B256) -> String {
    t.blocks
        .iter()
        .find(|(_, g)| g.block.hash() == hash)
        .map(|(l, _)| l.clone())
        .expect("a head that is not a known block")
}

/// Asserts that the state of `hash` equals `model` on every address the tree touched.
pub fn assert_state_of<S: Store + Sync>(
    chain: &Chain<S, xone_sync::testkit::VectorExecutor, Accept>,
    t: &Tree,
    label: &str,
) {
    use alloy_primitives::{B256, U256};
    use xone_state::{FlatRead, StateRead};
    let max = t.blocks.values().map(|g| g.block.number()).max().unwrap();
    let model = &t.blocks[label].flat;
    chain
        .reader()
        .with_state(t.hash(label), &mut |s: &dyn StateRead| {
            for a in &t.touched {
                assert_eq!(s.account(*a), model.account(*a), "account {a}");
            }
            for n in 1..=max {
                let slot = B256::from(U256::from(n));
                assert_eq!(
                    s.storage(counter(), slot),
                    model.storage(counter(), slot),
                    "slot {n}"
                );
            }
        })
        .unwrap();
}

/// The invariants of a consistent datadir: the head has its state, the index links to the anchor,
/// the floor is a canonical block.
pub fn consistent(chain: &MChain, t: &Tree) -> String {
    let (n, h) = chain.head();
    let label = label_of(t, h);
    assert_state_of(chain, t, &label);
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

pub fn labels_of(p: &Parts) -> BTreeSet<String> {
    p.t.blocks.keys().cloned().collect()
}
