#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)]
//! The persisted trie (`--state.disk`): rows flushed after random blocks, unwinds and restarts give
//! the roots of the in-memory trie and of `flat_root`, the row set is exactly that of a trie built
//! from the final state in one go (no stale row, none missing), every node but the root is a stub
//! after each flush, and a row that does not match its hash is refused.

use alloy_primitives::{B256, U256};
use proptest::prelude::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use xone_primitives::WideBalance;
use xone_state::{
    changeset::HashedDiff, trie::flat_root, Account, FlatRead, Flush, MemFlat, NodeRows, StateTrie,
};

#[derive(Default)]
struct MapRows(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl NodeRows for MapRows {
    fn node(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(key).cloned()
    }
}

impl MapRows {
    fn apply(&self, f: &Flush) {
        let mut m = self.0.lock().unwrap();
        for p in &f.del_prefix {
            m.retain(|k, _| !k.starts_with(p));
        }
        for k in &f.del {
            m.remove(k);
        }
        for (k, v) in &f.put {
            m.insert(k.clone(), v.clone());
        }
    }
    fn all(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.0.lock().unwrap().clone()
    }
}

/// Keys that share long prefixes (so there are extensions, deep branches and collapses).
fn key(a: u8, b: u8, c: u8) -> B256 {
    let mut k = [0u8; 32];
    k[0] = [0x00, 0x01, 0x10, 0x11][usize::from(a % 4)];
    k[1] = [0x00, 0x0f, 0xf0][usize::from(b % 3)];
    k[2] = c;
    k[31] = c.wrapping_mul(37);
    B256::from(k)
}

fn acc(n: u64) -> Account {
    Account {
        nonce: n,
        balance: WideBalance::from(U256::from(n * 1_000_003)),
        ..Account::default()
    }
}

#[derive(Debug, Clone)]
enum Op {
    Block(Vec<(u8, u8, u8, Option<u64>)>, Vec<(u8, u8, u8, u8, u8)>),
    Unwind,
    Restart,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (
            prop::collection::vec((0u8..4, 0u8..3, 0u8..12, prop::option::weighted(0.7, 1u64..4)), 0..8),
            prop::collection::vec((0u8..4, 0u8..3, 0u8..12, 0u8..40, 0u8..3), 0..24),
        ).prop_map(|(a, s)| Op::Block(a, s)),
        2 => Just(Op::Unwind),
        1 => Just(Op::Restart),
    ]
}

/// The rows of the whole state built in one go on an empty row set.
fn fresh_rows(flat: &MemFlat) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let rows = Arc::new(MapRows::default());
    let mut t = StateTrie::persisted(rows.clone(), alloy_trie::EMPTY_ROOT_HASH);
    let mut accounts = BTreeMap::new();
    let mut storage = BTreeMap::new();
    for (ha, a) in flat.hashed_accounts() {
        for (hs, v) in flat.hashed_slots(ha) {
            storage.insert((ha, hs), v);
        }
        accounts.insert(ha, Some(a));
    }
    t.apply(&accounts, &storage);
    let f = t.collect();
    rows.apply(&f);
    rows.all()
}

fn run(ops: Vec<Op>, depth: usize) {
    let rows = Arc::new(MapRows::default());
    let mut t =
        StateTrie::persisted(rows.clone(), alloy_trie::EMPTY_ROOT_HASH).with_prune_depth(depth);
    let mut mem = StateTrie::default();
    let mut flat = MemFlat::default();
    let mut stack: Vec<HashedDiff> = Vec::new();
    let mut root = alloy_trie::EMPTY_ROOT_HASH;
    for op in ops {
        match op {
            Op::Block(accounts, slots) => {
                let mut d = HashedDiff::default();
                for (a, b, c, v) in accounts {
                    d.accounts.insert(key(a, b, c), v.map(acc));
                }
                for (a, b, c, s, v) in slots {
                    d.storage
                        .insert((key(a, b, c), key(s, s / 4, s)), U256::from(v));
                }
                // a slot of an account that does not exist after the block is not part of any root
                let live = |ha: &B256| match d.accounts.get(ha) {
                    Some(a) => a.is_some(),
                    None => flat.hashed_account(*ha).is_some(),
                };
                let dead: Vec<_> = d
                    .storage
                    .keys()
                    .filter(|(a, _)| !live(a))
                    .copied()
                    .collect();
                for k in dead {
                    d.storage.remove(&k);
                }
                let cs = d.apply(&mut flat).unwrap();
                let (r1, _) = t.apply(&d.accounts, &d.storage);
                let (r2, _) = mem.apply(&d.accounts, &d.storage);
                assert_eq!(r1, r2, "persisted root equals the in-memory root");
                root = r1;
                stack.push(cs);
            }
            Op::Unwind => {
                if let Some(cs) = stack.pop() {
                    cs.revert(&mut flat);
                    let (r1, _) = t.apply(&cs.accounts, &cs.storage);
                    let (r2, _) = mem.apply(&cs.accounts, &cs.storage);
                    assert_eq!(r1, r2);
                    root = r1;
                }
            }
            Op::Restart => {
                t = StateTrie::persisted(rows.clone(), root).with_prune_depth(depth);
            }
        }
        assert_eq!(root, flat_root(&flat), "root equals the whole-table root");
        let f = t.collect();
        rows.apply(&f);
        t.finish();
        assert!(!t.failed());
        assert_eq!(
            rows.all(),
            fresh_rows(&flat),
            "rows are those of a fresh trie"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn persisted_trie_equals_in_memory_trie(ops in prop::collection::vec(op(), 1..20)) {
        run(ops.clone(), 0); // every node but the root is a stub after each flush
        run(ops.clone(), 2);
        run(ops, 5);
    }
}

#[test]
fn a_row_that_does_not_match_its_hash_is_refused() {
    let rows = Arc::new(MapRows::default());
    let mut t = StateTrie::persisted(rows.clone(), alloy_trie::EMPTY_ROOT_HASH).with_prune_depth(0);
    let mut accounts = BTreeMap::new();
    for i in 0..20u8 {
        accounts.insert(key(i, i, i), Some(acc(u64::from(i) + 1)));
    }
    let (root, _) = t.apply(&accounts, &BTreeMap::new());
    rows.apply(&t.collect());
    t.finish();
    // damage one row below the root
    {
        let mut m = rows.0.lock().unwrap();
        let k = m.keys().find(|k| k.len() > 2).unwrap().clone();
        let v = m.get_mut(&k).unwrap();
        let last = v.len() - 1;
        v[last] ^= 1;
    }
    let mut t = StateTrie::persisted(rows, root);
    let mut a2 = BTreeMap::new();
    for i in 0..20u8 {
        a2.insert(key(i, i, i), Some(acc(u64::from(i) + 100)));
    }
    t.apply(&a2, &BTreeMap::new());
    assert!(t.failed());
}
