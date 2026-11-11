#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the vectors are trusted inputs and a panic is the failure signal"
)]
//! A flood of 100,000 transactions against the pool bounds, with the memory it takes.
//!
//! The mix: regular executable and nonce-gapped transactions from thousands of senders, some of them close to
//! the 128 KiB limit, and sign transactions of candidates (which skip the fill checks). Every batch passes the
//! ADR-007 intake first (Q5 per peer, Q6 global). After every batch the executable count is at most
//! `global_slots`, the queue at most `global_queue` and no account queues more than `account_queue`.
//! The resident set is read from `/proc/self/status` (Linux), so this file holds one test and runs in its own process.

mod common;

use alloy_primitives::{Address, Bytes, B256, U256};
use common::{account, MapState};
use std::{sync::Arc, time::Instant};
use xone_builder::txpool::{
    propagate::Intake,
    special::{Authority, SIGN_SELECTOR},
    Config, Pool, PoolTx, TX_MAX_SIZE, TX_SLOT_SIZE,
};
use xone_chainspec::Network;
use xone_executor::tx::Tx;

const HEAD: u64 = 85_000_000;
const TIER: u128 = 625_000_000_000;
const TOTAL: usize = 100_000;
const SENDERS: u64 = 6000;
const SIGNERS: u64 = 150;

fn rss_kb(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let l = s.lines().find(|l| l.starts_with(field)).unwrap();
    l.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// xorshift: the flood is the same on every run.
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

fn sender(i: u64) -> Address {
    let mut a = [0u8; 20];
    a[12..].copy_from_slice(&i.to_be_bytes());
    Address::from(a)
}

fn mk(id: u64, from: Address, nonce: u64, price: u128, to: Address, data: Vec<u8>) -> Arc<PoolTx> {
    let mut h = [0u8; 32];
    h[24..].copy_from_slice(&id.to_be_bytes());
    let gas = 21_000 + 16 * u64::try_from(data.len()).unwrap() + 100_000;
    Arc::new(PoolTx {
        hash: B256::from(h),
        size: 110 + u64::try_from(data.len()).unwrap(),
        tx: Tx {
            ty: 0,
            chain_id: Some(51),
            nonce,
            gas_price: price,
            priority_fee: None,
            gas: gas.min(40_000_000),
            to: Some(to),
            value: U256::ZERO,
            data: Bytes::from(data),
            access_list: vec![],
            from,
        },
    })
}

#[test]
fn flood_of_100k_transactions_keeps_every_bound_and_bounded_memory() {
    let cfg = Config::default();
    let signers: Vec<Address> = (0..SIGNERS).map(|i| sender(1_000_000 + i)).collect();
    let mut st = MapState::default();
    let rich = U256::from(10u64).pow(U256::from(24));
    for i in 0..SENDERS {
        st.0.insert(sender(i), account(0, rich, &[]));
    }
    for a in &signers {
        st.0.insert(*a, account(0, U256::from(1u64), &[]));
    }
    let mut pool = Pool::new(
        Network::Apothem.spec(),
        &cfg,
        Authority::candidates(signers.iter().copied()),
        HEAD,
        50_000_000,
    )
    .unwrap();
    let base = rss_kb("VmRSS:");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut intake = Intake::default();
    let mut next_nonce = vec![0u64; usize::try_from(SENDERS).unwrap()];
    let mut next_sign = vec![0u64; usize::try_from(SIGNERS).unwrap()];
    let (mut offered, mut shed, mut ok, mut refused) = (0usize, 0usize, 0usize, 0usize);
    let mut peak_held = 0u64;
    let t0 = Instant::now();
    let mut id = 0u64;
    let (mut t_add, mut t_chk) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    while offered < TOTAL {
        let mut batch: Vec<(u64, Arc<PoolTx>)> = Vec::new();
        for _ in 0..400 {
            id += 1;
            offered += 1;
            let kind = rng.below(100);
            let peer = rng.below(64);
            let t = if kind < 5 {
                // a sign transaction of a candidate, at its pending nonce
                let k = usize::try_from(rng.below(SIGNERS)).unwrap();
                let mut d = SIGN_SELECTOR.to_vec();
                d.extend_from_slice(&U256::from(HEAD - rng.below(100)).to_be_bytes::<32>());
                d.extend_from_slice(&[1u8; 32]);
                let n = next_sign[k];
                next_sign[k] += 1;
                mk(id, signers[k], n, 0, xone_evm::route::BLOCK_SIGNERS, d)
            } else {
                let s = usize::try_from(rng.below(SENDERS)).unwrap();
                let gap = if kind < 70 { 0 } else { 1 + rng.below(40) };
                let n = next_nonce[s] + gap;
                if gap == 0 {
                    next_nonce[s] += 1;
                }
                let len = match rng.below(100) {
                    0..=3 => {
                        usize::try_from(TX_MAX_SIZE).unwrap()
                            - 200
                            - usize::try_from(rng.below(2000)).unwrap()
                    }
                    4..=15 => usize::try_from(TX_SLOT_SIZE).unwrap() + 100,
                    _ => usize::try_from(rng.below(400)).unwrap(),
                };
                mk(
                    id,
                    sender(u64::try_from(s).unwrap()),
                    n,
                    TIER + u128::from(rng.below(50_000_000_000)),
                    sender(9_999_999),
                    vec![0u8; len],
                )
            };
            if intake.admit(peer, t.size) {
                batch.push((peer, t));
            } else {
                shed += 1;
            }
        }
        let txs: Vec<Arc<PoolTx>> = batch.iter().map(|(_, t)| t.clone()).collect();
        let ta = Instant::now();
        let results = pool.add_batch(&st, 1_700_000_000, &txs);
        t_add += ta.elapsed();
        for r in results {
            if r.is_ok() {
                ok += 1;
            } else {
                refused += 1;
            }
        }
        for (peer, t) in &batch {
            intake.done(*peer, t.size);
        }
        // The bounds, after every batch.
        let tc = Instant::now();
        let (pending, queued) = pool.stats();
        assert!(
            pending <= usize::try_from(cfg.global_slots).unwrap(),
            "pending {pending}"
        );
        assert!(
            queued <= usize::try_from(cfg.global_queue).unwrap(),
            "queued {queued}"
        );
        let (p, q) = pool.content();
        assert!(
            q.values()
                .all(|l| l.len() <= usize::try_from(cfg.account_queue).unwrap()),
            "AccountQueue"
        );
        let held: u64 = p.values().chain(q.values()).flatten().map(|t| t.size).sum();
        peak_held = peak_held.max(held);
        let hard = (cfg.global_slots + cfg.global_queue) * TX_SLOT_SIZE * 4;
        assert!(held <= hard, "{held} bytes held");
        let _ = pool.take_new(); // the broadcast side drains it
        t_chk += tc.elapsed();
    }
    let hwm = rss_kb("VmHWM:");
    let (count, slots) = pool.len();
    // The worst case for memory: every slot taken by transactions at the 128 KiB limit, each dearer than the last,
    // so that each one evicts a cheaper one and the pool is full of the largest it can hold.
    let (mut worst_held, mut big) = (0u64, 0usize);
    for i in 0..3000u64 {
        let from = sender(SENDERS + 10 + i);
        st.0.insert(from, account(0, rich, &[]));
        let len = usize::try_from(TX_MAX_SIZE).unwrap() - 160;
        id += 1;
        let t = mk(
            id,
            from,
            0,
            TIER + 60_000_000_000 + u128::from(i) * 1_000_000,
            sender(9_999_999),
            vec![0u8; len],
        );
        assert!(t.size <= TX_MAX_SIZE);
        big += usize::from(pool.add_batch(&st, 1_700_000_000, &[t]).remove(0).is_ok());
        let _ = pool.take_new();
    }
    let (p, q) = pool.content();
    worst_held = worst_held.max(p.values().chain(q.values()).flatten().map(|t| t.size).sum());
    let hwm2 = rss_kb("VmHWM:");
    eprintln!(
        "worst case: {big} maximal transactions accepted; pool then holds {} txs, {} MiB of tx bytes; peak RSS {} MiB (+{} MiB over the start); bound: {} slots of 32 KiB = {} MiB",
        pool.len().0,
        worst_held / (1024 * 1024),
        hwm2 / 1024,
        hwm2.saturating_sub(base) / 1024,
        cfg.global_slots + cfg.global_queue,
        (cfg.global_slots + cfg.global_queue) * TX_SLOT_SIZE / (1024 * 1024)
    );
    assert!(
        worst_held <= (cfg.global_slots + cfg.global_queue) * TX_SLOT_SIZE,
        "{worst_held} bytes held"
    );
    assert!(
        hwm2.saturating_sub(base) < 400 * 1024,
        "peak RSS grew by {} KiB",
        hwm2 - base
    );
    eprintln!(
        "flood: {offered} offered, {shed} shed by intake (Q5/Q6), {ok} accepted, {refused} refused; pool holds {count} txs in {slots} slots, peak {} MiB of tx bytes; \
         {:.1}s (add_batch {:.1}s, bound checks {:.1}s); RSS before {} MiB, peak {} MiB (+{} MiB), after {} MiB",
        peak_held / (1024 * 1024),
        t0.elapsed().as_secs_f64(),
        t_add.as_secs_f64(),
        t_chk.as_secs_f64(),
        base / 1024,
        hwm / 1024,
        hwm.saturating_sub(base) / 1024,
        rss_kb("VmRSS:") / 1024
    );
    assert_eq!(offered, TOTAL);
    assert!(
        hwm.saturating_sub(base) < 512 * 1024,
        "peak RSS grew by {} KiB",
        hwm - base
    );
}
