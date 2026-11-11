#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! Consensus reads never run under the import lock (xOneGo PR 697: one lock across verification and
//! execution deadlocked at the first cold epoch switch). A cold verification of block B reads the
//! chain, then waits for something that only a concurrent insert of another block can give. If the
//! verification ran under the import lock, that insert could never start and B would wait for ever.

mod common;
use alloy_primitives::B256;
use common::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use xone_consensus::verify::HeaderError;
use xone_primitives::Header;
use xone_state::StateRead;
use xone_sync::chain::{Chain, ChainRead, HeaderCheck, Outcome};

const BOUND: Duration = Duration::from_secs(5);

/// A header check that is cold for one block: it derives its context from the chain in several
/// reads, with a pause between them in which another insert must be able to finish.
struct ColdSwitch {
    cold: B256,
    started: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
    reads: AtomicUsize,
}

impl HeaderCheck for ColdSwitch {
    fn check(
        &self,
        chain: &dyn ChainRead,
        header: &Header,
        parent: &Header,
        _: u64,
    ) -> Result<(), HeaderError> {
        if header.hash() != self.cold {
            return Ok(());
        }
        let read = || {
            chain.header(parent.hash()).unwrap();
            chain.canonical_hash(parent.number).unwrap();
            chain.committed();
            chain
                .with_state(parent.hash(), &mut |s: &dyn StateRead| {
                    s.account(counter());
                })
                .unwrap();
            self.reads.fetch_add(1, Ordering::SeqCst);
        };
        read();
        self.started.lock().unwrap().send(()).unwrap();
        // The store lock is free here, and so is the import lock.
        self.release
            .lock()
            .unwrap()
            .recv_timeout(BOUND)
            .map_err(|_| HeaderError::InvalidMixDigest)?;
        read();
        Ok(())
    }
}

#[test]
fn a_cold_epoch_switch_verifies_while_other_blocks_import() {
    for v2 in [true, false] {
        let mut w = World::new(v2, 1);
        for l in ["m2", "a2", "a3"] {
            w.make(l);
        }
        let cold = w.hash("m2");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let check = ColdSwitch {
            cold,
            started: Mutex::new(started_tx),
            release: Mutex::new(release_rx),
            reads: AtomicUsize::new(0),
        };
        let store = w.genesis_store(64, 4);
        let chain = Chain::new(w.spec.clone(), store, w.exec, check);
        let blocks: Vec<_> = ["s1", "m2", "a2", "a3"]
            .map(|l| w.blocks[l].block.clone())
            .into();
        chain.insert(blocks[0].clone(), 0).unwrap();
        let t0 = Instant::now();
        let (chain_ref, blocks_ref) = (&chain, &blocks);
        thread::scope(|sc| {
            let (cold_tx, cold_rx) = mpsc::channel();
            sc.spawn(move || {
                cold_tx
                    .send(chain_ref.insert(blocks_ref[1].clone(), 0))
                    .unwrap()
            });
            started_rx
                .recv_timeout(BOUND)
                .expect("the cold verification did not start");
            // While m2 is verifying (cold), other blocks import and the head moves.
            let (tx, rx) = mpsc::channel();
            sc.spawn(move || {
                tx.send((
                    chain_ref.insert(blocks_ref[2].clone(), 0),
                    chain_ref.insert(blocks_ref[3].clone(), 0),
                ))
                .unwrap()
            });
            let (a2, a3) = rx
                .recv_timeout(BOUND)
                .expect("an insert waited for a consensus read: deadlock");
            assert_eq!(a2, Ok(Outcome::Extended));
            assert_eq!(a3, Ok(Outcome::Extended));
            release_tx.send(()).unwrap();
            let r = cold_rx
                .recv_timeout(BOUND)
                .expect("the cold block never finished");
            // m2 is a sibling of the old head by now: V2 stores it, V1 adopts it (geth rules).
            assert!(r.is_ok(), "{r:?}");
        });
        assert!(t0.elapsed() < BOUND, "bounded time");
        // V2 keeps the head (m2 is not above it); V1 moves to the last block it was given.
        let want = if v2 { "a3" } else { "m2" };
        assert_eq!(chain.head().1, w.blocks[want].block.hash(), "v2 {v2}");
    }
}

/// The control: the same scenario with the verification under the lock deadlocks. The test above is
/// able to see the bug it is about.
#[test]
fn the_same_scenario_under_one_lock_deadlocks() {
    let lock = Mutex::new(0u32);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let lock_ref = &lock;
    thread::scope(|sc| {
        sc.spawn(move || {
            let mut g = lock_ref.lock().unwrap(); // insert lock held across verification
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(BOUND);
            *g += 1;
        });
        started_rx.recv_timeout(BOUND).unwrap();
        let (tx, rx) = mpsc::channel();
        sc.spawn(move || {
            *lock_ref.lock().unwrap() += 1;
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the second insert got the lock"
        );
        release_tx.send(()).unwrap();
        rx.recv_timeout(BOUND).unwrap();
    });
    assert_eq!(*lock.lock().unwrap(), 2);
}
