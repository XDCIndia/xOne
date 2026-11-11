#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    dead_code
)] // test support: a panic is the failure report
//! Synthetic block trees with real state roots. Every expected value is computed here from an
//! independent model (a plain `MemFlat` per block), or read from the geth oracle vectors.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_trie::EMPTY_ROOT_HASH;
use std::collections::{BTreeMap, BTreeSet};
use xone_chainspec::{devnet, ChainSpec};
use xone_consensus::verify::{Body, HeaderError};
use xone_primitives::{BlockInfo, ExtraFieldsV2, Header, QuorumCert};
use xone_state::{trie::flat_root, Account, FlatRead, MemFlat, StateDiff, StateRead};
use xone_sync::{
    chain::{Chain, ChainRead},
    executor::Execution,
    store::Block,
    testkit::{MemStore, VectorExecutor},
};

#[derive(serde::Deserialize)]
pub struct Event {
    pub block: String,
    pub err: String,
    pub head: String,
    pub canonical: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct Scenario {
    pub name: String,
    pub mode: String,
    pub fork: u64,
    pub main: u64,
    pub alt: u64,
    pub setup: Option<Vec<String>>,
    pub committed: String,
    pub events: Vec<Event>,
}

#[derive(serde::Deserialize)]
struct OracleFile {
    pin: String,
    scenarios: Vec<Scenario>,
}

/// The geth oracle's answers (`oracle/`, pinned in `oracle/PIN`).
pub fn scenarios() -> Vec<Scenario> {
    let f: OracleFile = serde_json::from_str(include_str!("../vectors/fork_choice.json")).unwrap();
    assert_eq!(f.pin, include_str!("../../oracle/PIN").trim());
    f.scenarios
}

pub type Accept = fn(&dyn ChainRead, &Header, &Header, u64) -> Result<(), HeaderError>;
pub type TestChain = Chain<MemStore, VectorExecutor, Accept>;

pub fn accept(_: &dyn ChainRead, _: &Header, _: &Header, _: u64) -> Result<(), HeaderError> {
    Ok(())
}

/// splitmix64: the only randomness in the tests, seeded per test.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub struct Gen {
    pub block: Block,
    pub flat: MemFlat,
}

/// A tree of blocks named as in the oracle: `s<n>` shared prefix, `m<n>` main, `a<n>` alt, with the
/// fork after `s<fork>`. `s0` is the anchor (block 0, empty state).
pub struct World {
    pub v2: bool,
    pub fork: u64,
    pub spec: ChainSpec,
    pub blocks: BTreeMap<String, Gen>,
    pub exec: VectorExecutor,
    pub touched: BTreeSet<Address>,
}

pub fn spec(v2: bool) -> ChainSpec {
    let mut s = devnet();
    if v2 {
        s.xdpos.v2.as_mut().unwrap().switch_block = 0;
    } else {
        s.xdpos.v2 = None;
    }
    s
}

pub fn counter() -> Address {
    Address::repeat_byte(0xc0)
}

impl World {
    pub fn new(v2: bool, fork: u64) -> Self {
        let mut w = Self {
            v2,
            fork,
            spec: spec(v2),
            blocks: BTreeMap::new(),
            exec: VectorExecutor::default(),
            touched: BTreeSet::from([counter()]),
        };
        let header = Header {
            state_root: EMPTY_ROOT_HASH,
            tx_root: EMPTY_ROOT_HASH,
            receipt_root: EMPTY_ROOT_HASH,
            uncle_hash: keccak256([0xc0]),
            difficulty: U256::from(1),
            ..Default::default()
        };
        w.blocks.insert(
            "s0".into(),
            Gen {
                block: Block::new(header, Body::default()),
                flat: MemFlat::default(),
            },
        );
        w
    }

    /// Parent label of `label` in the oracle's tree.
    pub fn parent_label(&self, label: &str) -> String {
        let (kind, n) = label.split_at(1);
        let n: u64 = n.parse().unwrap();
        if kind == "s" || n - 1 <= self.fork {
            format!("s{}", n - 1)
        } else {
            format!("{kind}{}", n - 1)
        }
    }

    /// Creates the block `label` (and its parents first).
    pub fn make(&mut self, label: &str) -> &Gen {
        if !self.blocks.contains_key(label) {
            let parent = self.parent_label(label);
            self.make(&parent);
            let g = self.build(label, &parent, |_| {});
            self.blocks.insert(label.to_owned(), g);
        }
        &self.blocks[label]
    }

    pub fn hash(&mut self, label: &str) -> B256 {
        self.make(label).block.hash()
    }

    pub fn block(&mut self, label: &str) -> Block {
        self.make(label).block.clone()
    }

    /// A variant of block `label` with an edited header, recorded with the honest execution of the
    /// original block (for the tests of the checks).
    pub fn variant(&mut self, label: &str, edit: impl FnOnce(&mut Header)) -> Block {
        self.make(label);
        let parent = self.parent_label(label);
        self.build(label, &parent, edit).block
    }

    fn build(&mut self, label: &str, parent: &str, edit: impl FnOnce(&mut Header)) -> Gen {
        let p = &self.blocks[parent];
        let n = p.block.number() + 1;
        let kind = label.as_bytes()[0];
        let user = Address::from_slice(&keccak256(label)[12..]);
        let slot = B256::from(U256::from(n));
        let pc = p.flat.account(counter());
        let mut diff = StateDiff::default();
        diff.accounts.insert(
            counter(),
            Some(Account {
                nonce: pc.as_ref().map_or(0, |a| a.nonce) + 1,
                balance: U256::from(n).into(),
                ..Default::default()
            }),
        );
        diff.accounts.insert(
            user,
            Some(Account {
                nonce: 1,
                balance: U256::from(n * 1000 + u64::from(kind)).into(),
                ..Default::default()
            }),
        );
        diff.storage
            .insert((counter(), slot), U256::from(n * 7 + u64::from(kind)));
        let pre_accounts = [counter(), user]
            .into_iter()
            .map(|a| (a, p.flat.account(a)))
            .collect();
        let pre_slots = BTreeMap::from([((counter(), slot), p.flat.storage(counter(), slot))]);
        let mut flat = p.flat.clone();
        diff.apply(&mut flat).unwrap();
        let root = flat_root(&flat);
        let mut h = Header {
            parent_hash: p.block.hash(),
            uncle_hash: keccak256([0xc0]),
            state_root: root,
            tx_root: EMPTY_ROOT_HASH,
            receipt_root: EMPTY_ROOT_HASH,
            difficulty: U256::from(1),
            number: n,
            gas_limit: 4_700_000,
            timestamp: 2 * n + u64::from(kind == b'a'),
            ..Default::default()
        };
        if self.v2 {
            h.extra_data = ExtraFieldsV2 {
                round: n,
                quorum_cert: QuorumCert {
                    proposed_block_info: BlockInfo {
                        hash: p.block.hash(),
                        round: n - 1,
                        number: n - 1,
                    },
                    signatures: vec![],
                    gap_number: 0,
                },
            }
            .encode_extra()
            .into();
        }
        edit(&mut h);
        let block = Block::new(h, Body::default());
        self.touched.insert(user);
        self.exec.record(
            block.hash(),
            pre_accounts,
            pre_slots,
            Execution {
                diff,
                receipts: vec![],
                state_root: root,
                gas_used: 0,
                access_list_hash: None,
            },
        );
        Gen { block, flat }
    }

    pub fn into_chain(self, store: MemStore) -> (TestChain, Tree) {
        let chain = Chain::new(self.spec, store, self.exec, accept as Accept);
        (
            chain,
            Tree {
                blocks: self.blocks,
                touched: self.touched,
            },
        )
    }

    /// A store anchored at block 0 (empty state).
    pub fn genesis_store(&self, retention: u64, blocks_per_file: u64) -> MemStore {
        let g = &self.blocks["s0"];
        MemStore::with_anchor(
            g.block.header.clone(),
            MemFlat::default(),
            retention,
            blocks_per_file,
        )
    }
}

/// The blocks of a [`World`] after its executor moved into a chain.
pub struct Tree {
    pub blocks: BTreeMap<String, Gen>,
    pub touched: BTreeSet<Address>,
}

impl Tree {
    pub fn block(&self, label: &str) -> Block {
        self.blocks[label].block.clone()
    }
    pub fn hash(&self, label: &str) -> B256 {
        self.blocks[label].block.hash()
    }
    pub fn assert_state(&self, chain: &TestChain, label: &str) {
        let max = self
            .blocks
            .values()
            .map(|g| g.block.number())
            .max()
            .unwrap();
        assert_state(
            chain,
            self.hash(label),
            &self.blocks[label].flat,
            &self.touched,
            max,
        );
    }
}

/// Asserts that the state of `hash` in `chain` equals `model` on every address the tree touched.
pub fn assert_state(
    chain: &TestChain,
    hash: B256,
    model: &MemFlat,
    touched: &BTreeSet<Address>,
    max: u64,
) {
    chain
        .reader()
        .with_state(hash, &mut |s: &dyn StateRead| {
            for a in touched {
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

/// A store with one planted bug, for the tests that show the vectors can see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// xOneGo 5c7be429, e8c845bd: a block is executed on the head's state, not its parent's.
    HeadState,
    /// A reorg that does not unwind (d4cdf136: the index kept the losing block below the new head).
    SkipUnwind,
    /// CertiK XDC-08: the committed block is forgotten, so a reorg may drop it.
    ForgetFloor,
}

pub struct Mutant {
    inner: MemStore,
    mode: Mode,
}

impl Mutant {
    pub fn new(inner: MemStore, mode: Mode) -> Self {
        Self { inner, mode }
    }
}

impl xone_sync::store::Store for Mutant {
    fn head(&self) -> (u64, B256) {
        self.inner.head()
    }
    fn canonical_hash(&self, n: u64) -> Option<B256> {
        self.inner.canonical_hash(n)
    }
    fn header(&self, h: B256) -> Option<Header> {
        self.inner.header(h)
    }
    fn record(&self, h: B256) -> Option<xone_sync::store::Record> {
        self.inner.record(h)
    }
    fn stateless(&self, h: B256) -> Option<Block> {
        self.inner.stateless(h)
    }
    fn state_at(&self, h: B256) -> Result<Box<dyn StateRead + '_>, xone_state::Missing> {
        let h = if self.mode == Mode::HeadState {
            self.inner.head().1
        } else {
            h
        };
        self.inner.state_at(h)
    }
    fn committed(&self) -> Option<xone_consensus::v2::commit::Link> {
        if self.mode == Mode::ForgetFloor {
            None
        } else {
            self.inner.committed()
        }
    }
    fn stateless_children(&self, p: B256) -> usize {
        self.inner.stateless_children(p)
    }
    fn durability(&self) -> xone_db::Durability {
        self.inner.durability()
    }
    fn commit_block(
        &mut self,
        r: &xone_sync::store::Record,
        c: Option<xone_consensus::v2::commit::Link>,
    ) -> Result<(), xone_sync::store::StoreError> {
        self.inner.commit_block(r, c)
    }
    fn unwind_to(
        &mut self,
        n: u64,
        b: xone_sync::store::Unwound,
    ) -> Result<(), xone_sync::store::StoreError> {
        if self.mode == Mode::SkipUnwind {
            return Ok(());
        }
        self.inner.unwind_to(n, b)
    }
    fn put_side(
        &mut self,
        r: &xone_sync::store::Record,
    ) -> Result<(), xone_sync::store::StoreError> {
        self.inner.put_side(r)
    }
    fn put_stateless(&mut self, b: &Block) -> Result<(), xone_sync::store::StoreError> {
        self.inner.put_stateless(b)
    }
    fn scan(&self) -> xone_sync::store::Scan {
        self.inner.scan()
    }
    fn repair(&mut self, a: &[xone_db::Action]) -> Result<(), xone_sync::store::StoreError> {
        self.inner.repair(a)
    }
}
