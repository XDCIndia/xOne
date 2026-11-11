#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! Values printed by the geth-XDC oracle (`oracle/run.sh`, pin in `oracle/PIN`); nothing here is
//! typed by hand. Also: the four genesis allocs against the chainspec's genesis state roots.

mod common;
use alloy_primitives::{keccak256, B256, U256};
use alloy_rlp::Decodable;
use common::*;
use serde::Deserialize;
use std::collections::BTreeSet;
use xone_chainspec::Network;
use xone_state::{
    trie::{flat_root, storage_root},
    Account, FlatRead, FlatWrite, MemFlat, StateDiff, StateProvider,
};

#[derive(Deserialize)]
struct AccVec {
    name: String,
    nonce: u64,
    balance: String,
    root: String,
    code_hash: String,
    full_rlp: String,
    slim_rlp: String,
    slim_to_full_rlp: String,
}

#[test]
fn account_leaf_and_slim_equal_geth() {
    let vecs: Vec<AccVec> = serde_json::from_str(include_str!("vectors/accounts.json")).unwrap();
    assert_eq!(vecs.len(), 24);
    for v in vecs {
        let acc = Account {
            nonce: v.nonce,
            balance: wide(&v.balance),
            storage_root: b256(&v.root),
            code_hash: b256(&v.code_hash),
        };
        assert_eq!(acc.rlp(), bytes(&v.full_rlp), "full {}", v.name);
        assert_eq!(acc.slim_rlp(), bytes(&v.slim_rlp), "slim {}", v.name);
        assert_eq!(
            Account::decode(&mut &bytes(&v.full_rlp)[..]).unwrap(),
            acc,
            "{}",
            v.name
        );
        let back = Account::from_slim_rlp(&bytes(&v.slim_rlp)).unwrap();
        assert_eq!(back, acc, "{}", v.name);
        assert_eq!(
            back.rlp(),
            bytes(&v.slim_to_full_rlp),
            "slim to full {}",
            v.name
        );
    }
}

#[test]
fn slim_refuses_odd_hash_lengths() {
    // list [0, 0, 0x01, ""]: a one-byte storage root
    let bad = [0xc4, 0x80, 0x80, 0x01, 0x80];
    assert_eq!(
        Account::from_slim_rlp(&bad),
        Err(xone_state::SlimError::BadHashLength)
    );
}

#[derive(Deserialize)]
struct Step {
    n: usize,
    op: String,
    addr: String,
    #[serde(default)]
    nonce: u64,
    #[serde(default)]
    balance: String,
    #[serde(default)]
    code_hash: String,
    #[serde(default)]
    slot: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    storage_root: String,
    root: String,
}

fn steps() -> Vec<Step> {
    serde_json::from_str(include_str!("vectors/mutations.json")).unwrap()
}

fn diff_of(s: &Step, flat: &MemFlat) -> StateDiff {
    let a = addr(&s.addr);
    let mut d = StateDiff::default();
    match s.op.as_str() {
        "put_account" => {
            let mut acc = flat.account(a).unwrap_or_default();
            acc.nonce = s.nonce;
            acc.balance = wide(&s.balance);
            acc.code_hash = b256(&s.code_hash);
            d.accounts.insert(a, Some(acc));
        }
        "put_slot" => {
            d.storage.insert((a, b256(&s.slot)), word(&s.value));
        }
        "del_account" => {
            d.accounts.insert(a, None);
        }
        op => panic!("{op}"),
    }
    d
}

#[test]
fn fifty_mutations_root_equals_geth_at_every_step() {
    let steps = steps();
    assert_eq!(steps.len(), 50);
    // The three boundary balances are in the script, and so are all three ops.
    let bal: Vec<_> = steps
        .iter()
        .filter(|s| s.op == "put_account")
        .map(|s| wide(&s.balance))
        .collect();
    let (ones, zeros) = ("f".repeat(64), "0".repeat(64));
    for want in [
        ones.clone(),
        format!("1{zeros}"),
        format!("1{}1", &zeros[1..]),
    ] {
        assert!(
            bal.contains(&wide(&want)),
            "boundary balance missing: {want}"
        );
    }
    assert!(["put_account", "put_slot", "del_account"]
        .iter()
        .all(|o| steps.iter().any(|s| s.op == *o)));

    let mut flat = MemFlat::default();
    for s in &steps {
        diff_of(s, &flat).apply(&mut flat).unwrap();
        assert_eq!(
            flat_root(&flat),
            b256(&s.root),
            "state root after step {} ({})",
            s.n,
            s.op
        );
        if !s.storage_root.is_empty() {
            assert_eq!(
                storage_root(flat.slots(addr(&s.addr))),
                b256(&s.storage_root),
                "storage root, step {}",
                s.n
            );
        }
    }
}

#[test]
fn fifty_mutations_unwind_to_every_step_root_and_open_every_block() {
    let steps = steps();
    let mut flat = MemFlat::default();
    let mut snaps = vec![flat.clone()]; // snaps[k]: state after k blocks
    let mut p = StateProvider::new(MemFlat::default(), 0, B256::ZERO, 64);
    let hash = |k: usize| keccak256([k as u8, 0xb1]);
    let mut parent = B256::ZERO;
    for (k, s) in steps.iter().enumerate() {
        let d = diff_of(s, &flat);
        d.apply(&mut flat).unwrap();
        snaps.push(flat.clone());
        p.commit_block(hash(k + 1), parent, &d).unwrap();
        parent = hash(k + 1);
    }
    let addrs: BTreeSet<_> = steps.iter().map(|s| addr(&s.addr)).collect();
    let slots: BTreeSet<_> = steps
        .iter()
        .filter(|s| !s.slot.is_empty())
        .map(|s| b256(&s.slot))
        .collect();
    use xone_state::StateRead;
    for (k, snap) in snaps.iter().enumerate() {
        let h = if k == 0 { B256::ZERO } else { hash(k) };
        let v = p.at(h).unwrap();
        for a in &addrs {
            assert_eq!(v.account(*a), snap.account(*a), "block {k} account");
            for s in &slots {
                assert_eq!(
                    v.storage(*a, *s),
                    snaps[k].storage(*a, *s),
                    "block {k} slot"
                );
            }
        }
    }
    for k in (0..50).rev() {
        p.unwind_to(k as u64).unwrap();
        let want = if k == 0 {
            alloy_trie::EMPTY_ROOT_HASH
        } else {
            b256(&steps[k - 1].root)
        };
        assert_eq!(flat_root(p.flat()), want, "root after unwinding to {k}");
        assert_eq!(p.flat(), &snaps[k]);
    }
}

#[test]
fn genesis_alloc_roots_equal_the_chainspec_roots() {
    for net in Network::ALL {
        let g = net.genesis().unwrap();
        let mut flat = MemFlat::default();
        for (a, acc) in &g.alloc {
            flat.set_account(
                *a,
                Some(Account {
                    nonce: acc.nonce,
                    balance: acc.balance.into(),
                    code_hash: if acc.code.is_empty() {
                        alloy_primitives::KECCAK256_EMPTY
                    } else {
                        keccak256(&acc.code)
                    },
                    ..Default::default()
                }),
            );
            for (slot, v) in &acc.storage {
                flat.set_slot(*a, *slot, U256::from_be_bytes(v.0));
            }
        }
        assert_eq!(
            flat_root(&flat),
            net.spec().genesis_state_root,
            "{}",
            net.name()
        );
    }
}

/// Mutant: a leaf that carries only the wrapped part (what `TrieAccount` would write) is caught.
#[test]
fn narrowing_a_wide_balance_changes_the_root() {
    let steps = steps();
    let mut flat = MemFlat::default();
    for s in &steps[..3] {
        diff_of(s, &flat).apply(&mut flat).unwrap();
    }
    assert_eq!(flat_root(&flat), b256(&steps[2].root));
    let a = addr(&steps[1].addr); // 2^256: wrapped value is 0
    let mut acc = flat.account(a).unwrap();
    assert!(acc.balance.is_wide());
    acc.balance = acc.balance.low().into();
    flat.set_account(a, Some(acc));
    assert_ne!(flat_root(&flat), b256(&steps[2].root));
}
