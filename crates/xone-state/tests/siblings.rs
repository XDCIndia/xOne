//! Real data. `tests/vectors/sibling_diffs.json` holds, for net2040 619,712 and 619,713 and Apothem
//! 85,209,326 and 85,214,323, the block-level state diff read from the xdc02 archives
//! (`tools/fetch_sibling_diffs.py`: prestateTracer diffMode merged over the block's transactions,
//! plus the EIP-2935 write and the fee owner, checked against debug_getModifiedAccountsByNumber).
//!
//! WHAT THIS CAN AND CANNOT SHOW. A canonical sibling root needs the full trie of the parent, which
//! the archive does not hand out; a diff of touched accounts cannot produce a `stateRoot`. So these
//! tests do NOT check any sibling's canonical root. They check the weaker, exact property that the
//! data allows: the canonical diff of a block applied on the parent's touched-account subset leaves
//! the parent unchanged when it is opened again, and the views of two blocks over one parent are
//! independent objects. `storage_root` is not in a trace: it is left at its default in every record,
//! and the comparisons use the same records on both sides.
//! Note also that 619,713 is the child of 619,712 and 85,214,323 is far from 85,209,326, so these
//! pairs are two blocks over one merged parent subset, not two blocks with one real parent hash.

mod common;
use alloy_primitives::{Address, B256};
use common::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use xone_state::{Account, FlatRead, FlatWrite, MemFlat, StateDiff, StateProvider, StateRead};

#[derive(Deserialize, Clone)]
struct Rec {
    nonce: u64,
    balance: String,
    code_hash: String,
    storage: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Pair {
    pre: Option<Rec>,
    post: Option<Rec>,
}

#[derive(Deserialize)]
struct Block {
    network: String,
    number: u64,
    hash: String,
    parent_hash: String,
    accounts: BTreeMap<String, Pair>,
}

fn account(r: &Rec) -> Account {
    Account {
        nonce: r.nonce,
        balance: wide(&r.balance),
        code_hash: b256(&r.code_hash),
        ..Default::default()
    }
}

/// (parent subset, canonical diff) of one block.
fn parts(b: &Block) -> (MemFlat, StateDiff) {
    let (mut pre, mut diff) = (MemFlat::default(), StateDiff::default());
    for (a, p) in &b.accounts {
        let a = addr(a);
        pre.set_account(a, p.pre.as_ref().map(account));
        diff.accounts.insert(a, p.post.as_ref().map(account));
        for (s, v) in p.pre.iter().flat_map(|r| &r.storage) {
            pre.set_slot(a, b256(s), word(v));
        }
        for (s, v) in p.post.iter().flat_map(|r| &r.storage) {
            diff.storage.insert((a, b256(s)), word(v));
        }
    }
    (pre, diff)
}

fn blocks() -> Vec<Block> {
    let v: Vec<Block> = serde_json::from_str(include_str!("vectors/sibling_diffs.json")).unwrap();
    assert_eq!(
        v.iter()
            .map(|b| (b.network.as_str(), b.number))
            .collect::<Vec<_>>(),
        [
            ("net2040", 619712),
            ("net2040", 619713),
            ("apothem", 85209326),
            ("apothem", 85214323)
        ]
    );
    assert!(v.iter().all(|b| !b.accounts.is_empty()));
    v
}

fn keys(f: &MemFlat, d: &StateDiff) -> (Vec<Address>, Vec<(Address, B256)>) {
    let mut accts: Vec<_> = f
        .accounts()
        .map(|(a, _)| a)
        .chain(d.accounts.keys().copied())
        .collect();
    let mut slots: Vec<_> = d.storage.keys().copied().collect();
    for (a, _) in f.accounts().collect::<Vec<_>>() {
        slots.extend(f.slots(a).map(|(s, _)| (a, s)));
    }
    accts.sort();
    accts.dedup();
    slots.sort();
    slots.dedup();
    (accts, slots)
}

#[test]
fn canonical_diff_then_reopening_the_parent_gives_the_untouched_parent_values_not_a_root_check() {
    for b in blocks() {
        let (pre, diff) = parts(&b);
        let (accts, slots) = keys(&pre, &diff);
        let (hash, parent) = (b256(&b.hash), b256(&b.parent_hash));
        let mut p = StateProvider::new(pre.clone(), b.number - 1, parent, 4);
        let reads = |r: &dyn StateRead| {
            (
                accts.iter().map(|a| r.account(*a)).collect::<Vec<_>>(),
                slots
                    .iter()
                    .map(|(a, s)| r.storage(*a, *s))
                    .collect::<Vec<_>>(),
            )
        };
        let parent_values = reads(&p.at(parent).unwrap());
        // the parent's values are the diff's pre side
        assert_eq!(
            parent_values.0,
            accts.iter().map(|a| pre.account(*a)).collect::<Vec<_>>()
        );

        // 1. as a side block: the child shows the post values, the parent view and the head do not change
        let child = p.at(parent).unwrap().child(&diff);
        let post_values = reads(&child);
        assert_ne!(
            post_values, parent_values,
            "{} {}: the block changes something",
            b.network, b.number
        );
        assert_eq!(reads(&p.at(parent).unwrap()), parent_values);
        assert_eq!(p.flat(), &pre);

        // 2. as the canonical block: head = post, parent still opens to the pre values, unwind restores
        p.commit_block(hash, parent, &diff).unwrap();
        assert_eq!(reads(&p.at(hash).unwrap()), post_values);
        assert_eq!(reads(&p.at(parent).unwrap()), parent_values);
        p.unwind_to(b.number - 1).unwrap();
        assert_eq!(p.flat(), &pre);
        assert_eq!(reads(&p.at(parent).unwrap()), parent_values);
    }
}

#[test]
fn views_of_two_blocks_over_one_merged_parent_subset_are_independent_not_a_root_check() {
    let bs = blocks();
    for pair in [(&bs[0], &bs[1]), (&bs[2], &bs[3])] {
        let ((pre_a, da), (pre_b, db)) = (parts(pair.0), parts(pair.1));
        // merged parent subset: the first block's values win where both touch a key
        let mut merged = pre_b.clone();
        for (a, acc) in pre_a.accounts() {
            merged.set_account(a, Some(acc));
        }
        for (a, _) in pre_a.accounts().collect::<Vec<_>>() {
            for (s, v) in pre_a.slots(a) {
                merged.set_slot(a, s, v);
            }
        }
        let parent = b256(&pair.0.parent_hash);
        let mut p = StateProvider::new(merged.clone(), pair.0.number - 1, parent, 4);
        p.insert_side(b256(&pair.0.hash), parent, da.clone())
            .unwrap();
        p.insert_side(b256(&pair.1.hash), parent, db.clone())
            .unwrap();
        let (va, vb) = (
            p.at(b256(&pair.0.hash)).unwrap(),
            p.at(b256(&pair.1.hash)).unwrap(),
        );
        let vp = p.at(parent).unwrap();
        for (v, d, other) in [(&va, &da, &db), (&vb, &db, &da)] {
            for (a, post) in &d.accounts {
                assert_eq!(v.account(*a), *post);
            }
            for ((a, s), x) in &d.storage {
                assert_eq!(v.storage(*a, *s), *x);
            }
            // a key only the other block touches reads as the parent value
            for a in other
                .accounts
                .keys()
                .filter(|a| !d.accounts.contains_key(*a))
            {
                assert_eq!(v.account(*a), vp.account(*a));
            }
            for k in other.storage.keys().filter(|k| !d.storage.contains_key(*k)) {
                assert_eq!(v.storage(k.0, k.1), vp.storage(k.0, k.1));
            }
        }
        let (accts, slots) = keys(&merged, &StateDiff::default());
        for a in accts {
            assert_eq!(vp.account(a), merged.account(a));
        }
        for (a, s) in slots {
            assert_eq!(vp.storage(a, s), merged.storage(a, s));
        }
        assert_eq!(p.flat(), &merged);
    }
}
