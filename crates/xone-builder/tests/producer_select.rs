#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Transaction selection (geth `fillTransactions` and `commitTransactions`) and the sign transaction the
//! producer sends: special transactions first, the price order, the gas pool, the gas cap, the sign filters,
//! and a signed sign transaction that the pool decodes with the node's address as sender.

mod producer_common;

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};
use producer_common::{KeySealer, TestKey};
use xone_builder::{
    producer::{
        build::{select, Cand, Commit, Params},
        signtx,
    },
    txpool::PoolTx,
};
use xone_chainspec::Network;
use xone_evm::route::BLOCK_SIGNERS;

const CHAIN: u64 = 51;
const GWEI: u128 = 1_000_000_000;

fn params(gas_limit: u64) -> Params {
    Params {
        number: 100,
        gas_limit,
        soft_gas_cap: 0,
        base_fee: Some(12 * GWEI),
        max_sign_txs: 4,
    }
}

type Queued = Vec<(Address, Vec<(std::sync::Arc<PoolTx>, u64)>)>;

fn pending(txs: Queued) -> BTreeMap<Address, Vec<Cand>> {
    txs.into_iter()
        .map(|(a, v)| {
            (
                a,
                v.into_iter().map(|(tx, seen)| Cand { tx, seen }).collect(),
            )
        })
        .collect()
}

fn declared(t: &PoolTx) -> Commit {
    Commit::Used(t.tx.gas)
}

fn order(out: &[std::sync::Arc<PoolTx>]) -> Vec<(Address, u64)> {
    out.iter().map(|t| (t.from(), t.tx.nonce)).collect()
}

fn sign_data(number: u64) -> Vec<u8> {
    signtx::calldata(number, B256::with_last_byte(1)).to_vec()
}

#[test]
fn special_transactions_go_first_and_outside_the_price_check() {
    let (a, b, s) = (TestKey::new(1), TestKey::new(2), TestKey::new(3));
    let to = Address::repeat_byte(0xee);
    let p = pending(vec![
        (
            a.address(),
            vec![(a.tx(CHAIN, 0, 900 * GWEI, 21_000, to, &[]), 1)],
        ),
        (
            b.address(),
            vec![(b.tx(CHAIN, 0, 625 * GWEI, 21_000, to, &[]), 2)],
        ),
        // gas price 0 is below the base fee: a paid transaction like it is refused, a sign transaction goes first
        (
            s.address(),
            vec![(s.tx(CHAIN, 0, 0, 200_000, BLOCK_SIGNERS, &sign_data(50)), 3)],
        ),
    ]);
    let out = select(p, &params(10_000_000), declared);
    assert_eq!(
        order(&out),
        [(s.address(), 0), (a.address(), 0), (b.address(), 0)]
    );
    // the same transaction from an account that is not special is dropped by the base fee check
    let p = pending(vec![(
        a.address(),
        vec![(a.tx(CHAIN, 0, 0, 21_000, to, &[]), 1)],
    )]);
    assert!(select(p, &params(10_000_000), declared).is_empty());
}

#[test]
fn a_special_transaction_behind_a_paid_nonce_waits_for_it() {
    let (a, to) = (TestKey::new(1), Address::repeat_byte(0xee));
    let p = pending(vec![(
        a.address(),
        vec![
            (a.tx(CHAIN, 0, 20 * GWEI, 21_000, to, &[]), 1),
            (a.tx(CHAIN, 1, 0, 200_000, BLOCK_SIGNERS, &sign_data(50)), 2),
        ],
    )]);
    let out = select(p, &params(10_000_000), declared);
    assert_eq!(
        order(&out),
        [(a.address(), 0), (a.address(), 1)],
        "nonce order holds"
    );
}

#[test]
fn price_order_then_first_seen_then_nonce_order_per_sender() {
    let (a, b, c) = (TestKey::new(1), TestKey::new(2), TestKey::new(3));
    let to = Address::repeat_byte(0xee);
    let p = pending(vec![
        (
            a.address(),
            vec![
                (a.tx(CHAIN, 0, 30 * GWEI, 21_000, to, &[]), 5),
                (a.tx(CHAIN, 1, 90 * GWEI, 21_000, to, &[]), 5),
            ],
        ),
        (
            b.address(),
            vec![(b.tx(CHAIN, 0, 30 * GWEI, 21_000, to, &[]), 4)],
        ),
        (
            c.address(),
            vec![(c.tx(CHAIN, 0, 40 * GWEI, 21_000, to, &[]), 9)],
        ),
    ]);
    let out = select(p, &params(10_000_000), declared);
    // c (40) first; a0 and b0 tie at 30: b was seen first; then a0's successor, whatever its price, follows a0
    assert_eq!(
        order(&out),
        [
            (c.address(), 0),
            (b.address(), 0),
            (a.address(), 0),
            (a.address(), 1)
        ]
    );
}

#[test]
fn a_transaction_that_does_not_fit_skips_its_account_not_the_block() {
    let (a, b, c) = (TestKey::new(1), TestKey::new(2), TestKey::new(3));
    let to = Address::repeat_byte(0xee);
    let p = pending(vec![
        (
            a.address(),
            vec![(a.tx(CHAIN, 0, 50 * GWEI, 21_000, to, &[]), 1)],
        ),
        (
            b.address(),
            vec![(b.tx(CHAIN, 0, 40 * GWEI, 40_000, to, &[]), 2)],
        ),
        (
            c.address(),
            vec![(c.tx(CHAIN, 0, 30 * GWEI, 21_000, to, &[]), 3)],
        ),
    ]);
    let out = select(p, &params(50_000), declared);
    assert_eq!(order(&out), [(a.address(), 0), (c.address(), 0)]);
}

#[test]
fn the_soft_gas_cap_stops_the_block_and_the_last_transaction_may_pass_it() {
    let to = Address::repeat_byte(0xee);
    let keys: Vec<_> = (1..=4).map(TestKey::new).collect();
    let p = |ks: &[TestKey]| {
        pending(
            ks.iter()
                .enumerate()
                .map(|(i, k)| {
                    (
                        k.address(),
                        vec![(
                            k.tx(CHAIN, 0, (50 - i as u128) * GWEI, 21_000, to, &[]),
                            i as u64,
                        )],
                    )
                })
                .collect(),
        )
    };
    let mut q = params(10_000_000);
    q.soft_gas_cap = 30_000; // after two transactions the gas used is 42,000, past the cap
    assert_eq!(select(p(&keys), &q, declared).len(), 2);
    q.soft_gas_cap = 0;
    assert_eq!(select(p(&keys), &q, declared).len(), 4);
}

#[test]
fn sign_transactions_with_short_data_or_for_a_future_block_are_skipped_and_the_block_is_capped() {
    let keys: Vec<_> = (1..=7).map(TestKey::new).collect();
    let sign = |k: &TestKey, data: &[u8], seen: u64| {
        (
            k.address(),
            vec![(k.tx(CHAIN, 0, 0, 200_000, BLOCK_SIGNERS, data), seen)],
        )
    };
    let mut txs = vec![
        sign(&keys[0], &sign_data(50)[..40], 1), // calldata too short
        sign(&keys[1], &sign_data(100), 2),      // signs block 100, the block being built
    ];
    // five valid ones, each of its own sender: the cap is four
    for (i, k) in keys[2..].iter().enumerate() {
        txs.push(sign(k, &sign_data(50 + i as u64), 10 + i as u64));
    }
    let out = select(pending(txs), &params(100_000_000), declared);
    assert!(out
        .iter()
        .all(|t| t.from() != keys[0].address() && t.from() != keys[1].address()));
    assert_eq!(out.len(), 4, "capped at max_sign_txs, the first seen first");
    assert_eq!(out[0].from(), keys[2].address());
}

#[test]
fn an_invalid_transaction_drops_the_rest_of_its_sender_and_a_low_nonce_only_itself() {
    let (a, b, to) = (TestKey::new(1), TestKey::new(2), Address::repeat_byte(0xee));
    let mk = || {
        pending(vec![
            (
                a.address(),
                vec![
                    (a.tx(CHAIN, 0, 40 * GWEI, 21_000, to, &[]), 1),
                    (a.tx(CHAIN, 1, 40 * GWEI, 21_000, to, &[]), 1),
                ],
            ),
            (
                b.address(),
                vec![
                    (b.tx(CHAIN, 0, 30 * GWEI, 21_000, to, &[]), 1),
                    (b.tx(CHAIN, 1, 30 * GWEI, 21_000, to, &[]), 1),
                ],
            ),
        ])
    };
    let (af, bf) = (a.address(), b.address());
    let out = select(mk(), &params(10_000_000), |t| {
        if t.from() == af {
            Commit::Invalid
        } else if t.tx.nonce == 0 {
            Commit::NonceTooLow
        } else {
            Commit::Used(21_000)
        }
    });
    assert_eq!(
        order(&out),
        [(bf, 1)],
        "a is dropped, b's low nonce is skipped, its next goes in"
    );
}

#[test]
fn the_sign_transaction_is_signed_through_the_unit_and_decodes_to_the_node_address() {
    let spec = Network::Apothem.spec();
    let mut u = KeySealer(TestKey::new(9));
    let hash = B256::with_last_byte(0x42);
    let raw = signtx::emit(&spec, 30, hash, 31, true, 7, Some(9), &mut u)
        .unwrap()
        .expect("block 30 is a sign block");
    let tx = PoolTx::decode(&raw, spec.chain_id).unwrap();
    assert_eq!(tx.from(), u.0.address());
    assert_eq!(
        (tx.tx.nonce, tx.tx.gas, tx.tx.gas_price, tx.tx.to),
        (9, 200_000, 0, Some(BLOCK_SIGNERS)),
        "the state nonce is the floor"
    );
    assert_eq!(tx.tx.data, signtx::calldata(30, hash));
    // not a sign block, a stale block, a non-candidate: no transaction
    assert!(signtx::emit(&spec, 31, hash, 32, true, 0, None, &mut u)
        .unwrap()
        .is_none());
    assert!(signtx::emit(&spec, 30, hash, 61, true, 0, None, &mut u)
        .unwrap()
        .is_none());
    assert!(signtx::emit(&spec, 30, hash, 31, false, 0, None, &mut u)
        .unwrap()
        .is_none());
    // a penalised candidate still signs: the gate is the candidate pool, not the masternode list
    assert!(signtx::emit(&spec, 45, hash, 46, true, 0, None, &mut u)
        .unwrap()
        .is_some());
}
