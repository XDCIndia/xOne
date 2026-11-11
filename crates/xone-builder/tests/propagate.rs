#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Announce, broadcast and fetch as pure functions: constants and behaviour of geth's `BroadcastTransactions`,
//! `syncTransactions` and `TxFetcher`, driven by a fixed clock.
use alloy_primitives::{Address, Bytes, B256, U256};
use std::sync::Arc;
use xone_builder::txpool::{
    propagate::{
        broadcast, sync_new_peer, Announced, Delivered, Effect, Fetcher, PeerState, Wire,
        MAX_ANNOUNCE_HASHES, MAX_QUEUED_TXS, MAX_TX_ANNOUNCES, MAX_TX_BROADCAST_SIZE,
        MAX_TX_RETRIEVALS, MAX_TX_RETRIEVAL_SIZE, TX_ARRIVE_TIMEOUT_MS, TX_FETCH_TIMEOUT_MS,
    },
    PoolTx,
};
use xone_executor::tx::Tx;

fn h(n: u64) -> B256 {
    let mut b = [0u8; 32];
    b[24..].copy_from_slice(&n.to_be_bytes());
    B256::from(b)
}

fn tx(n: u64, size: u64) -> Arc<PoolTx> {
    Arc::new(PoolTx {
        hash: h(n),
        size,
        tx: Tx {
            ty: 0,
            chain_id: None,
            nonce: n,
            gas_price: 1,
            priority_fee: None,
            gas: 21_000,
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: vec![],
            from: Address::repeat_byte(1),
        },
    })
}

fn peers(n: u64, wire: Wire) -> Vec<PeerState> {
    (1..=n).map(|i| PeerState::new(i, wire)).collect()
}

const KEY: [u8; 16] = [3; 16];

fn whole(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| matches!(e, Effect::Transactions { .. }))
        .count()
}

#[test]
fn a_new_tx_goes_whole_to_ceil_sqrt_of_the_peers_and_by_hash_to_the_rest() {
    let mut ps = peers(10, Wire::Announce);
    let fx = broadcast(&mut ps, &[tx(1, 200)], &KEY, 99);
    assert_eq!(whole(&fx), 4, "ceil(sqrt(10))");
    let ann: usize = fx
        .iter()
        .filter(|e| matches!(e, Effect::Announce { .. }))
        .count();
    assert_eq!(ann, 6);
    // Everyone now knows it: nothing is sent twice.
    assert!(broadcast(&mut ps, &[tx(1, 200)], &KEY, 99).is_empty());
    // The same sender's next transaction goes to the same peers (the score depends on the sender).
    let who = |fx: &[Effect]| -> Vec<u64> {
        fx.iter()
            .filter_map(|e| {
                if let Effect::Transactions { peer, .. } = e {
                    Some(*peer)
                } else {
                    None
                }
            })
            .collect()
    };
    let again = broadcast(&mut ps, &[tx(2, 200)], &KEY, 99);
    let first = who(&fx);
    let mut a = who(&again);
    let mut b = first;
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
}

#[test]
fn xdc_wire_peers_get_no_announcement_and_a_large_tx_is_never_sent_whole() {
    let mut ps = peers(9, Wire::Full);
    let fx = broadcast(&mut ps, &[tx(1, 200)], &KEY, 1);
    assert_eq!(
        fx.len(),
        3,
        "only the 3 chosen peers get a message; eth/100 and eth/101 have no announce"
    );
    assert!(fx.iter().all(|e| matches!(e, Effect::Transactions { .. })));
    assert!(
        ps.iter().all(|p| p.known.contains(&h(1))),
        "the rest are marked known, as geth does"
    );
    let mut ps = peers(9, Wire::Announce);
    let fx = broadcast(&mut ps, &[tx(2, MAX_TX_BROADCAST_SIZE + 1)], &KEY, 1);
    assert_eq!(whole(&fx), 0);
    assert_eq!(fx.len(), 9, "announced to everyone");
    // exactly at the limit still goes whole
    assert_eq!(
        whole(&broadcast(
            &mut peers(9, Wire::Announce),
            &[tx(3, MAX_TX_BROADCAST_SIZE)],
            &KEY,
            1
        )),
        3
    );
}

#[test]
fn queues_and_packets_are_bounded() {
    let mut ps = peers(1, Wire::Announce);
    let many: Vec<_> = (0..(MAX_QUEUED_TXS as u64 + 500))
        .map(|i| tx(i, 100))
        .collect();
    let fx = broadcast(&mut ps, &many, &KEY, 1);
    let sent: usize = fx
        .iter()
        .map(|e| {
            if let Effect::Transactions { hashes, .. } = e {
                hashes.len()
            } else {
                0
            }
        })
        .sum();
    assert_eq!(
        sent, MAX_QUEUED_TXS,
        "older broadcasts are dropped past the queue cap"
    );
    // packets of at least 100 KiB
    let mut ps = peers(1, Wire::Announce);
    let big: Vec<_> = (0..10).map(|i| tx(i, 4000)).collect();
    let fx = broadcast(&mut ps, &big, &KEY, 1);
    let sizes: Vec<usize> = fx
        .iter()
        .map(|e| {
            if let Effect::Transactions { hashes, .. } = e {
                hashes.len()
            } else {
                0
            }
        })
        .collect();
    assert_eq!(sizes, vec![10], "40 kB is one packet");
    let mut ps = peers(1, Wire::Announce);
    let anns: Vec<_> = (0..(MAX_ANNOUNCE_HASHES as u64 + 10))
        .map(|i| tx(i, MAX_TX_BROADCAST_SIZE + 1))
        .collect();
    let fx = broadcast(&mut ps, &anns, &KEY, 1);
    assert_eq!(
        fx.len(),
        2,
        "an announcement holds at most {MAX_ANNOUNCE_HASHES} hashes"
    );
}

#[test]
fn a_new_peer_is_sent_the_pending_pool() {
    let pending: Vec<_> = (0..5).map(|i| tx(i, 300)).collect();
    let mut xdc = PeerState::new(1, Wire::Full);
    let fx = sync_new_peer(&mut xdc, &pending);
    assert_eq!(
        fx,
        vec![Effect::Transactions {
            peer: 1,
            hashes: pending.iter().map(|t| t.hash).collect()
        }],
        "an XDC peer gets them whole (the pin sends it nothing)"
    );
    assert!(sync_new_peer(&mut xdc, &pending).is_empty(), "once");
    let mut modern = PeerState::new(2, Wire::Announce);
    assert!(
        matches!(&sync_new_peer(&mut modern, &pending)[..], [Effect::Announce { hashes, .. }] if hashes.len() == 5)
    );
}

fn ann(n: u64, size: u32) -> Announced {
    Announced {
        hash: h(n),
        kind: 0,
        size,
    }
}

#[test]
fn an_announced_hash_waits_for_the_broadcast_then_is_fetched_from_one_peer() {
    let mut f = Fetcher::new();
    assert!(f
        .notify(0, 1, &[ann(1, 100), ann(2, 100)], |_| false)
        .is_empty());
    assert!(
        f.tick(TX_ARRIVE_TIMEOUT_MS - 101).is_empty(),
        "still inside the arrival window"
    );
    // hash 2 arrives by broadcast meanwhile
    let (more, bad) = f.delivered(
        300,
        7,
        &[Delivered {
            hash: h(2),
            kind: 0,
            size: 100,
        }],
        false,
    );
    assert!(more.is_empty() && bad.is_empty());
    let fx = f.tick(TX_ARRIVE_TIMEOUT_MS);
    assert_eq!(
        fx,
        vec![Effect::Fetch {
            peer: 1,
            hashes: vec![h(1)]
        }]
    );
    // one request per peer: a second announcement from the same peer waits
    assert!(f
        .notify(TX_ARRIVE_TIMEOUT_MS, 1, &[ann(3, 100)], |_| false)
        .is_empty());
    assert!(
        f.tick(2 * TX_ARRIVE_TIMEOUT_MS + 1).is_empty(),
        "peer 1 has a request out"
    );
    // a known hash is not tracked at all
    let mut g = Fetcher::new();
    g.notify(0, 1, &[ann(9, 1)], |hh| *hh == h(9));
    assert_eq!(g.tracked(), 0);
}

#[test]
fn a_request_holds_at_most_256_hashes_and_128_kib_of_announced_bytes() {
    let mut f = Fetcher::new();
    let all: Vec<_> = (0..1000).map(|i| ann(i, 10)).collect();
    f.notify(0, 1, &all, |_| false);
    let Effect::Fetch { hashes, .. } = &f.tick(TX_ARRIVE_TIMEOUT_MS)[0] else {
        panic!()
    };
    assert_eq!(hashes.len(), MAX_TX_RETRIEVALS);
    assert_eq!(hashes[0], h(0), "in the order announced");
    let mut f = Fetcher::new();
    let fat: Vec<_> = (0..10).map(|i| ann(i, 60_000)).collect();
    f.notify(0, 1, &fat, |_| false);
    let Effect::Fetch { hashes, .. } = &f.tick(TX_ARRIVE_TIMEOUT_MS)[0] else {
        panic!()
    };
    assert_eq!(
        hashes.len(),
        3,
        "the third announced tx takes the total past {MAX_TX_RETRIEVAL_SIZE} bytes"
    );
}

#[test]
fn one_peer_cannot_have_more_than_4096_hashes_outstanding() {
    let mut f = Fetcher::new();
    let all: Vec<_> = (0..(MAX_TX_ANNOUNCES as u64 + 100))
        .map(|i| ann(i, 1))
        .collect();
    f.notify(0, 1, &all, |_| false);
    assert_eq!(f.tracked(), MAX_TX_ANNOUNCES);
    f.notify(0, 1, &[ann(99_999, 1)], |_| false);
    assert_eq!(f.tracked(), MAX_TX_ANNOUNCES, "over the cap: dropped");
}

#[test]
fn an_unanswered_request_moves_to_the_other_announcer_after_five_seconds() {
    let mut f = Fetcher::new();
    f.notify(0, 1, &[ann(1, 100)], |_| false);
    f.notify(10, 2, &[ann(1, 100)], |_| false);
    let fx = f.tick(TX_ARRIVE_TIMEOUT_MS);
    assert_eq!(fx.len(), 1);
    let Effect::Fetch { peer: first, .. } = fx[0].clone() else {
        panic!()
    };
    assert!(f
        .tick(TX_ARRIVE_TIMEOUT_MS + TX_FETCH_TIMEOUT_MS - 200)
        .is_empty());
    let fx = f.tick(TX_ARRIVE_TIMEOUT_MS + TX_FETCH_TIMEOUT_MS);
    let [Effect::Fetch {
        peer: second,
        hashes,
    }] = &fx[..]
    else {
        panic!("{fx:?}")
    };
    assert_ne!(*second, first);
    assert_eq!(hashes, &vec![h(1)]);
    assert!(f.next_deadline().is_some());
}

#[test]
fn a_delivery_that_contradicts_its_announcement_names_the_peer() {
    let mut f = Fetcher::new();
    f.notify(0, 1, &[ann(1, 100)], |_| false);
    f.tick(TX_ARRIVE_TIMEOUT_MS);
    let (_, bad) = f.delivered(
        600,
        1,
        &[Delivered {
            hash: h(1),
            kind: 0,
            size: 109,
        }],
        true,
    );
    assert_eq!(bad, vec![1], "9 bytes off");
    let mut f = Fetcher::new();
    f.notify(0, 1, &[ann(1, 100)], |_| false);
    f.tick(TX_ARRIVE_TIMEOUT_MS);
    let (_, bad) = f.delivered(
        600,
        1,
        &[Delivered {
            hash: h(1),
            kind: 0,
            size: 108,
        }],
        true,
    );
    assert!(bad.is_empty(), "8 bytes off is allowed");
    let mut f = Fetcher::new();
    f.notify(0, 1, &[ann(1, 100)], |_| false);
    f.tick(TX_ARRIVE_TIMEOUT_MS);
    let (_, bad) = f.delivered(
        600,
        1,
        &[Delivered {
            hash: h(1),
            kind: 2,
            size: 100,
        }],
        true,
    );
    assert_eq!(bad, vec![1], "wrong type");
}

#[test]
fn dropping_a_peer_gives_its_hashes_to_the_others() {
    let mut f = Fetcher::new();
    f.notify(0, 1, &[ann(1, 100)], |_| false);
    f.notify(5, 2, &[ann(1, 100)], |_| false);
    let fx = f.tick(TX_ARRIVE_TIMEOUT_MS);
    let Effect::Fetch { peer, .. } = fx[0].clone() else {
        panic!()
    };
    let fx = f.drop_peer(TX_ARRIVE_TIMEOUT_MS + 1, peer);
    assert!(matches!(&fx[..], [Effect::Fetch { peer: p, .. }] if *p != peer));
}
