#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)]
//! The hash-keyed flat tables give the root of an independent address-keyed model: random diffs
//! (writes, zero writes, deletions with storage), the root after each, and unwinds back.

use alloy_primitives::{Address, B256, U256};
use proptest::prelude::*;
use std::collections::BTreeMap;
use xone_state::{
    trie::{flat_root, root_after, state_root, storage_root},
    Account, FlatRead, MemFlat, StateDiff, StateProvider,
};

/// The reference: plain maps, roots from the address-keyed trie functions (they hash inside).
#[derive(Default, Clone)]
struct Model {
    accounts: BTreeMap<Address, Account>,
    storage: BTreeMap<(Address, B256), U256>,
}

impl Model {
    fn apply(&mut self, d: &StateDiff) {
        for (a, x) in &d.accounts {
            match x {
                Some(x) => {
                    self.accounts.insert(*a, x.clone());
                }
                None => {
                    self.accounts.remove(a);
                    self.storage.retain(|(b, _), _| b != a);
                }
            }
        }
        for (&(a, s), &v) in &d.storage {
            if matches!(d.accounts.get(&a), Some(None)) {
                continue;
            }
            if v.is_zero() {
                self.storage.remove(&(a, s));
            } else {
                self.storage.insert((a, s), v);
            }
        }
    }

    fn root(&self) -> B256 {
        state_root(self.accounts.iter().map(|(a, x)| {
            let mut x = x.clone();
            x.storage_root = storage_root(
                self.storage
                    .range((*a, B256::ZERO)..=(*a, B256::repeat_byte(0xff)))
                    .map(|(&(_, s), &v)| (s, v)),
            );
            (*a, x)
        }))
    }
}

fn addr(i: u8) -> Address {
    Address::repeat_byte(i)
}

fn diff() -> impl Strategy<Value = StateDiff> {
    (
        proptest::collection::btree_map(0u8..12, proptest::option::of(1u64..50), 0..6),
        proptest::collection::btree_map((0u8..12, 0u8..8), 0u64..4, 0..10),
    )
        .prop_map(|(accs, slots)| StateDiff {
            accounts: accs
                .into_iter()
                .map(|(a, n)| {
                    (
                        addr(a),
                        n.map(|nonce| Account {
                            nonce,
                            ..Default::default()
                        }),
                    )
                })
                .collect(),
            storage: slots
                .into_iter()
                .map(|((a, s), v)| ((addr(a), B256::repeat_byte(s)), U256::from(v)))
                .collect(),
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn hashed_roots_equal_the_address_keyed_model_and_unwind_restores_them(
        diffs in proptest::collection::vec(diff(), 1..8)
    ) {
        let mut model = Model::default();
        let mut roots = vec![model.root()];
        let mut p = StateProvider::new(MemFlat::default(), 0, B256::ZERO, 16);
        let mut parent = B256::ZERO;
        for (i, d) in diffs.iter().enumerate() {
            // the executor never writes a slot of an account that does not exist after the block
            let mut d = d.clone();
            d.storage.retain(|(a, _), _| match d.accounts.get(a) {
                Some(x) => x.is_some(),
                None => model.accounts.contains_key(a),
            });
            let d = &d;
            // the root a diff would give, before it is written
            let before = model.clone();
            model.apply(d);
            roots.push(model.root());
            prop_assert_eq!(root_after(p.flat(), d), model.root());
            let h = B256::repeat_byte(u8::try_from(i + 1).unwrap());
            use xone_state::StateRead;
            prop_assert_eq!(p.at(parent).unwrap().root_with(d).unwrap(), model.root());
            p.commit_block(h, parent, d).unwrap();
            parent = h;
            prop_assert_eq!(flat_root(p.flat()), model.root());
            prop_assert_eq!(p.root(), model.root());
            // reads by address agree with the model
            for a in 0u8..12 {
                prop_assert_eq!(p.flat().account(addr(a)), model.accounts.get(&addr(a)).cloned());
                let _ = &before;
            }
        }
        for n in (0..diffs.len()).rev() {
            p.unwind_to(u64::try_from(n).unwrap()).unwrap();
            prop_assert_eq!(flat_root(p.flat()), roots[n]);
            prop_assert_eq!(p.root(), roots[n]);
        }
    }
}

/// The streamed root of sorted rows is `flat_root` of the same rows: slots of accounts with and
/// without storage, a zero slot, and slots of an absent account.
#[test]
fn the_root_of_sorted_rows_equals_flat_root() {
    use alloy_primitives::keccak256;
    use xone_state::{trie::root_of_sorted_rows, FlatWrite};
    let mut flat = MemFlat::default();
    let (mut accounts, mut storage) = (Vec::new(), Vec::new());
    for i in 0..300u32 {
        let h = keccak256(i.to_be_bytes());
        let a = Account {
            nonce: u64::from(i),
            ..Default::default()
        };
        flat.set_hashed_account(h, Some(a.clone()));
        accounts.push((h, a));
        if i % 3 == 0 {
            for j in 0..(i % 40) {
                let (k, v) = (
                    keccak256([i.to_be_bytes(), j.to_be_bytes()].concat()),
                    U256::from(j + 1),
                );
                flat.set_hashed_slot(h, k, v);
                storage.push((h, k, v));
            }
        }
    }
    let orphan = keccak256(b"absent");
    storage.push((orphan, keccak256(b"slot"), U256::from(9u8)));
    storage.push((accounts[0].0, keccak256(b"zero"), U256::ZERO));
    accounts.sort_by_key(|(h, _)| *h);
    storage.sort_by_key(|(a, k, _)| (*a, *k));
    assert_eq!(root_of_sorted_rows(&accounts, &storage), flat_root(&flat));
    assert_eq!(
        root_of_sorted_rows(&[], &[]),
        flat_root(&MemFlat::default())
    );
}

#[test]
fn the_streamed_roots_equal_the_row_roots() {
    use xone_state::trie::{root_of_sorted_rows, state_root_sorted, storage_root_sorted};
    let mut accounts = Vec::new();
    let mut storage = Vec::new();
    for i in 0..40u8 {
        let ha = B256::repeat_byte(i.wrapping_mul(7).wrapping_add(1));
        accounts.push((ha, Account::default()));
        for j in 0..(i % 5) {
            storage.push((
                ha,
                B256::repeat_byte(j + 1),
                alloy_primitives::U256::from(u64::from(j) + 1),
            ));
        }
    }
    accounts.sort_by_key(|(h, _)| *h);
    accounts.dedup_by_key(|(h, _)| *h);
    storage.sort_by_key(|(a, k, _)| (*a, *k));
    assert_eq!(
        state_root_sorted(accounts.clone(), storage.clone()),
        root_of_sorted_rows(&accounts, &storage)
    );
    assert_eq!(state_root_sorted([], []), alloy_trie::EMPTY_ROOT_HASH);
    let slots: Vec<_> = (1..6u8)
        .map(|j| (B256::repeat_byte(j), alloy_primitives::U256::from(j)))
        .collect();
    assert_eq!(
        storage_root_sorted(slots.clone()),
        xone_state::trie::storage_root_hashed(slots)
    );
}
