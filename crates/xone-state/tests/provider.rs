use alloy_primitives::{keccak256, Address, B256, U256};
use xone_state::{
    Account, CommitError, MemFlat, Missing, StateDiff, StateProvider, StateRead, StateView,
};

fn h(k: u8) -> B256 {
    keccak256([k, 0x77])
}

fn put(n: u64) -> StateDiff {
    let mut d = StateDiff::default();
    d.accounts.insert(
        Address::with_last_byte(1),
        Some(Account {
            nonce: n,
            ..Default::default()
        }),
    );
    d.storage.insert(
        (Address::with_last_byte(1), B256::with_last_byte(1)),
        U256::from(n),
    );
    d
}

fn chain(retention: u64, blocks: u8) -> StateProvider<MemFlat> {
    let mut p = StateProvider::new(MemFlat::default(), 0, h(0), retention);
    for k in 1..=blocks {
        p.commit_block(h(k), h(k - 1), &put(k as u64)).unwrap();
    }
    p
}

#[test]
fn outside_the_retained_window_is_a_typed_missing() {
    let mut p = chain(3, 6); // heads 6; blocks 3..=6 open
    assert_eq!(p.head(), (6, h(6)));
    assert!(matches!(p.at(h(2)), Err(Missing::Block(x)) if x == h(2)));
    assert!(matches!(p.at(h(99)), Err(Missing::Block(_))));
    assert_eq!(
        p.at(h(3))
            .unwrap()
            .account(Address::with_last_byte(1))
            .unwrap()
            .nonce,
        3
    );
    assert_eq!(p.at(h(3)).unwrap().number(), 3);
    assert_eq!(p.unwind_to(2), Err(Missing::Number(2)));
    assert_eq!(p.unwind_to(7), Err(Missing::Number(7)));
    p.unwind_to(4).unwrap();
    assert_eq!(p.head(), (4, h(4)));
    assert!(p.at(h(5)).is_err());
}

#[test]
fn commit_needs_the_head_as_parent_and_a_new_hash() {
    let mut p = chain(8, 2);
    assert_eq!(
        p.commit_block(h(3), h(1), &put(3)),
        Err(CommitError::NotExtending {
            head: h(2),
            parent: h(1)
        })
    );
    assert_eq!(
        p.commit_block(h(2), h(2), &put(3)),
        Err(CommitError::Known(h(2)))
    );
    assert_eq!(
        p.insert_side(h(9), h(42), put(9)),
        Err(CommitError::Missing(Missing::Block(h(42))))
    );
    assert_eq!(
        p.insert_side(h(1), h(0), put(9)),
        Err(CommitError::Known(h(1)))
    );
}

#[test]
fn side_branch_stacks_on_a_side_parent_and_head_does_not_move() {
    let mut p = chain(8, 4);
    let head_before = p.flat().clone();
    // side chain S1 -> S2 on block 2 (the head is block 4)
    let mut s1 = StateDiff::default();
    s1.accounts.insert(Address::with_last_byte(1), None); // delete: storage must read zero
    let mut s2 = StateDiff::default();
    s2.accounts.insert(
        Address::with_last_byte(2),
        Some(Account {
            nonce: 5,
            ..Default::default()
        }),
    );
    p.insert_side(h(20), h(2), s1).unwrap();
    p.insert_side(h(21), h(20), s2).unwrap();
    let (a1, slot) = (Address::with_last_byte(1), B256::with_last_byte(1));
    let v2 = p.at(h(21)).unwrap();
    assert_eq!(v2.account(a1), None);
    assert_eq!(v2.storage(a1, slot), U256::ZERO);
    assert_eq!(v2.account(Address::with_last_byte(2)).unwrap().nonce, 5);
    // the parent of the side chain and the head are untouched
    assert_eq!(p.at(h(2)).unwrap().storage(a1, slot), U256::from(2));
    assert_eq!(p.at(h(4)).unwrap().storage(a1, slot), U256::from(4));
    assert_eq!(p.flat(), &head_before);
    // unwinding the canonical chain below the side base makes the side chain unopenable
    p.unwind_to(1).unwrap();
    assert!(matches!(p.at(h(21)), Err(Missing::Block(x)) if x == h(2)));
}

#[test]
fn views_are_independent_children_of_one_parent() {
    let p = chain(8, 3);
    let (da, db) = (put(100), put(200));
    let (a, slot) = (Address::with_last_byte(1), B256::with_last_byte(1));
    let va = p.at(h(3)).unwrap().child(&da);
    let vb = p.at(h(3)).unwrap().child(&db);
    assert_eq!(va.account(a).unwrap().nonce, 100);
    assert_eq!(vb.account(a).unwrap().nonce, 200);
    assert_eq!(va.storage(a, slot), U256::from(100));
    assert_eq!(vb.storage(a, slot), U256::from(200));
    assert_eq!(p.at(h(3)).unwrap().storage(a, slot), U256::from(3));
}

#[test]
fn readers_share_a_view_across_threads() {
    fn sync<T: Send + Sync>(_: &T) {}
    let p = chain(8, 5);
    let v: StateView<'_, MemFlat> = p.at(h(2)).unwrap();
    sync(&v);
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                for _ in 0..100 {
                    assert_eq!(
                        v.storage(Address::with_last_byte(1), B256::with_last_byte(1)),
                        U256::from(2)
                    );
                }
            });
        }
    });
}
