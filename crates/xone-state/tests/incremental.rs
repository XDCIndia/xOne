#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! The incremental trie against the reference root over the whole table: after each of 10,000 random
//! diffs (accounts created, changed and deleted with their storage, slots set and cleared, wide
//! balances), before each commit (`root_with`, including a diff that is asked for and then not
//! committed), and after random unwinds.

use alloy_primitives::{keccak256, Address, B256, U256};
use xone_primitives::WideBalance;
use xone_state::{
    trie::flat_root, Account, FlatRead, MemFlat, StateDiff, StateProvider, StateRead,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn addr(i: u64) -> Address {
    Address::from_word(keccak256(i.to_be_bytes()))
}

fn account(r: &mut Rng) -> Account {
    let balance = match r.below(8) {
        0 => WideBalance::default(),
        1 => WideBalance::from(U256::MAX),
        2 => {
            let mut b = vec![1u8];
            b.extend([0; 32]);
            b[32] = r.below(256) as u8;
            WideBalance::from_be_bytes(&b)
        }
        _ => WideBalance::from(U256::from(r.next())),
    };
    Account {
        nonce: r.below(5),
        balance,
        code_hash: keccak256([r.below(3) as u8]),
        ..Default::default()
    }
}

fn gen(r: &mut Rng, flat: &MemFlat) -> StateDiff {
    let mut d = StateDiff::default();
    for _ in 0..r.below(6) {
        let a = addr(r.below(48));
        if r.below(5) == 0 {
            if flat.account(a).is_some() {
                d.accounts.insert(a, None);
            }
        } else {
            d.accounts.insert(a, Some(account(r)));
        }
    }
    for _ in 0..r.below(10) {
        let a = addr(r.below(48));
        let exists = match d.accounts.get(&a) {
            Some(x) => x.is_some(),
            None => flat.account(a).is_some(),
        };
        if !exists {
            continue;
        }
        let v = match r.below(3) {
            0 => U256::ZERO,
            1 => U256::from(r.below(4) + 1),
            _ => U256::from_limbs([r.next(), r.next(), r.next(), r.next()]),
        };
        d.storage
            .insert((a, B256::with_last_byte(r.below(24) as u8)), v);
    }
    d
}

#[test]
fn incremental_root_equals_reference_after_10000_diffs_and_unwinds() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let h = |k: u64| keccak256(k.to_be_bytes());
    // a non-empty start: the trie is built from the flat tables once
    let mut seed = MemFlat::default();
    let init = {
        let mut d = StateDiff::default();
        for i in 0..20 {
            d.accounts.insert(addr(i), Some(account(&mut r)));
            d.storage
                .insert((addr(i), B256::with_last_byte(1)), U256::from(i + 1));
        }
        d
    };
    init.apply(&mut seed).unwrap();
    let mut p = StateProvider::new(seed, 0, h(0), 1_000_000);
    assert_eq!(p.root(), flat_root(p.flat()));
    let (mut n, mut unwinds, mut deepest) = (0u64, 0, 0);
    while n < 10_000 {
        let d = gen(&mut r, p.flat());
        let mut want = p.flat().clone();
        d.apply(&mut want).unwrap();
        let want = flat_root(&want);
        let (head, hash) = p.head();
        if r.below(7) == 0 {
            // asked for and not committed: the trie must not keep it
            let other = gen(&mut r, p.flat());
            p.at(hash).unwrap().root_with(&other).unwrap();
        }
        assert_eq!(
            p.at(hash).unwrap().root_with(&d).unwrap(),
            want,
            "root_with at {head}"
        );
        p.commit_block(h(n + 1), hash, &d).unwrap();
        n += 1;
        assert_eq!(p.root(), want, "root after block {n}");
        if r.below(200) == 0 {
            let back = 1 + r.below(150.min(n));
            let to = p.head().0 - back;
            p.unwind_to(to).unwrap();
            assert_eq!(p.root(), flat_root(p.flat()), "root after unwind to {to}");
            n = to;
            unwinds += 1;
            deepest = deepest.max(back);
        }
    }
    assert!(
        unwinds > 10 && deepest > 30,
        "unwinds {unwinds}, deepest {deepest}"
    );
}
