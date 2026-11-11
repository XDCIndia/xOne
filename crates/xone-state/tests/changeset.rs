//! Changeset properties: apply N then revert N restores the state and the root; every retained
//! block opens to exactly the state it had; a side block's view equals the diff applied to a copy.

use alloy_primitives::{keccak256, Address, B256, U256};
use proptest::prelude::*;
use xone_primitives::{WideBalance, WideBalanceError};
use xone_state::{
    trie::flat_root, Account, FlatRead, MemFlat, StateDiff, StateProvider, StateRead,
};

fn balance() -> impl Strategy<Value = WideBalance> {
    let two256 = |lo: u8| {
        let mut b = vec![1u8];
        b.extend([0; 31]);
        b.push(lo);
        WideBalance::from_be_bytes(&b)
    };
    prop_oneof![
        Just(WideBalance::default()),
        Just(WideBalance::from(U256::MAX)),
        Just(two256(0)),
        Just(two256(1)),
        any::<u64>().prop_map(|v| WideBalance::from(U256::from(v))),
        any::<[u8; 33]>().prop_map(|b| WideBalance::from_be_bytes(&b)), // up to 2^264-1
    ]
}

fn account() -> impl Strategy<Value = Account> {
    (0u64..4, balance(), 0u8..2).prop_map(|(nonce, balance, c)| Account {
        nonce,
        balance,
        code_hash: keccak256([c]),
        ..Default::default()
    })
}

fn a(i: u8) -> Address {
    Address::with_last_byte(i)
}

fn s(i: u8) -> B256 {
    B256::with_last_byte(i)
}

fn diff() -> impl Strategy<Value = StateDiff> {
    (
        prop::collection::vec((0u8..4, prop::option::weighted(0.7, account())), 0..4),
        prop::collection::vec(((0u8..4, 0u8..4), 0u8..3), 0..6),
    )
        .prop_map(|(accts, slots)| StateDiff {
            accounts: accts.into_iter().map(|(i, x)| (a(i), x)).collect(),
            storage: slots
                .into_iter()
                .map(|((i, j), v)| ((a(i), s(j)), U256::from(v)))
                .collect(),
        })
}

fn probe(r: &impl StateRead) -> Vec<(Option<Account>, Vec<U256>)> {
    (0..4)
        .map(|i| {
            (
                r.account(a(i)),
                (0..4).map(|j| r.storage(a(i), s(j))).collect(),
            )
        })
        .collect()
}

struct Flat<'a>(&'a MemFlat);
impl StateRead for Flat<'_> {
    fn account(&self, x: Address) -> Option<Account> {
        self.0.account(x)
    }
    fn storage(&self, x: Address, y: B256) -> U256 {
        self.0.storage(x, y)
    }
}

proptest! {
    #[test]
    fn apply_n_then_revert_n_restores_state_and_root(base in diff(), diffs in prop::collection::vec(diff(), 1..8)) {
        let mut flat = MemFlat::default();
        base.apply(&mut flat).unwrap();
        let (orig, orig_root) = (flat.clone(), flat_root(&flat));
        let mut done = Vec::new();
        for d in &diffs {
            let before = (flat.clone(), flat_root(&flat));
            done.push((d.apply(&mut flat).unwrap(), before));
        }
        while let Some((cs, (state, root))) = done.pop() {
            cs.revert(&mut flat);
            prop_assert_eq!(&flat, &state);
            prop_assert_eq!(flat_root(&flat), root);
        }
        prop_assert_eq!(&flat, &orig);
        prop_assert_eq!(flat_root(&flat), orig_root);
    }

    #[test]
    fn every_retained_block_opens_to_its_state_and_unwinds_to_it(diffs in prop::collection::vec(diff(), 1..10)) {
        let h = |k: usize| keccak256([k as u8]);
        let mut p = StateProvider::new(MemFlat::default(), 0, h(0), 16);
        let mut model = MemFlat::default();
        let mut snaps = vec![model.clone()];
        for (k, d) in diffs.iter().enumerate() {
            d.apply(&mut model).unwrap();
            snaps.push(model.clone());
            p.commit_block(h(k + 1), h(k), d).unwrap();
        }
        for (k, snap) in snaps.iter().enumerate() {
            prop_assert_eq!(probe(&p.at(h(k)).unwrap()), probe(&Flat(snap)), "open block {}", k);
        }
        for k in (0..snaps.len()).rev() {
            p.unwind_to(k as u64).unwrap();
            prop_assert_eq!(p.flat(), &snaps[k]);
            prop_assert_eq!(flat_root(p.flat()), flat_root(&snaps[k]));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]
    /// 1,000 generated sibling pairs: A and B on one parent. Each view equals the diff applied to a
    /// copy of the parent, the parent view is unchanged, and the head state is untouched.
    #[test]
    fn generated_sibling_pairs_on_one_parent(base in diff(), mid in diff(), da in diff(), db in diff()) {
        let h = |k: u8| keccak256([k, 0x51]);
        let mut p = StateProvider::new(MemFlat::default(), 0, h(0), 8);
        let mut parent = MemFlat::default();
        base.apply(&mut parent).unwrap();
        p.commit_block(h(1), h(0), &base).unwrap();
        mid.apply(&mut parent).unwrap();
        p.commit_block(h(2), h(1), &mid).unwrap();
        // A and B are both children of block 2; the head is block 2 too.
        p.insert_side(h(10), h(2), da.clone()).unwrap();
        p.insert_side(h(11), h(2), db.clone()).unwrap();
        let head_before = p.flat().clone();
        let (mut ma, mut mb) = (parent.clone(), parent.clone());
        da.apply(&mut ma).unwrap();
        db.apply(&mut mb).unwrap();
        prop_assert_eq!(probe(&p.at(h(10)).unwrap()), probe(&Flat(&ma)));
        prop_assert_eq!(probe(&p.at(h(11)).unwrap()), probe(&Flat(&mb)));
        prop_assert_eq!(probe(&p.at(h(2)).unwrap()), probe(&Flat(&parent)));
        prop_assert_eq!(p.flat(), &head_before);
        prop_assert_eq!(p.head().1, h(2));
    }
}

#[test]
fn illegal_balance_is_refused_before_any_write() {
    let mut over = vec![1u8];
    over.extend([0; 33]); // 2^264: 265 bits
    let bad = Account {
        balance: WideBalance::from_be_bytes(&over),
        ..Default::default()
    };
    let mut flat = MemFlat::default();
    let mut d = StateDiff::default();
    d.accounts.insert(a(1), Some(Account::default()));
    d.accounts.insert(a(2), Some(bad));
    assert_eq!(d.apply(&mut flat), Err(WideBalanceError::AboveBound));
    assert_eq!(flat, MemFlat::default());
}
