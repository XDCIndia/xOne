//! Operator surfaces (XB-21): ethstats, SkyNet heartbeat, Prometheus metrics and logs.
//!
//! Everything reads the node through the two traits below and owns its own thread, so a slow
//! or dead telemetry server never blocks import or consensus (the traits are plain reads,
//! the node implements them over its own snapshots). No module here keeps global state; the
//! tracing subscriber installed by [`logs`] is the single exception.

pub mod ethstats;
pub mod heartbeat;
pub mod logs;
pub mod metrics;
mod wire;

/// One block as the stats board shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub number: u64,
    pub hash: [u8; 32],
    pub parent_hash: [u8; 32],
    pub timestamp: u64,
    pub miner: [u8; 20],
    pub gas_used: u64,
    pub gas_limit: u64,
    pub difficulty: u64,
    pub tx_hashes: Vec<[u8; 32]>,
    pub tx_root: [u8; 32],
    pub state_root: [u8; 32],
}

/// Chain and pool view. Implementations must answer from memory, never wait on a lock held
/// by import.
pub trait ChainStats: Send + Sync {
    fn head(&self) -> Option<BlockInfo>;
    fn block(&self, number: u64) -> Option<BlockInfo>;
    fn syncing(&self) -> bool;
    fn gas_price(&self) -> u64;
    fn pending_txs(&self) -> usize;
    /// The address this node is authorised for AND actively sealing with; None for a
    /// follower. Read live, never from a flag (xOneGo e2560360 / 6acb9280).
    fn signer(&self) -> Option<[u8; 20]>;
}

pub trait PeerStats: Send + Sync {
    fn peer_count(&self) -> usize;
}

/// Lowercase 0x-prefixed hex, as geth's `common.Hash` and `common.Address` marshal.
pub fn hex0x(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 + 2 * b.len());
    s.push_str("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

    pub struct Fake {
        pub head: AtomicU64,
        pub syncing: AtomicBool,
        pub peers: AtomicU64,
        pub signer: Option<[u8; 20]>,
        /// Changes the head hash without changing the number.
        pub salt: AtomicU64,
    }
    impl Fake {
        pub fn new(head: u64) -> Self {
            Self {
                head: AtomicU64::new(head),
                syncing: AtomicBool::new(false),
                peers: AtomicU64::new(9),
                signer: None,
                salt: AtomicU64::new(0),
            }
        }
    }
    pub fn block(n: u64, txs: usize) -> BlockInfo {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        h[1] = 1;
        let mut p = [0u8; 32];
        p[0] = 0xcd;
        p[1] = 2;
        let mut r = [0u8; 32];
        r[0] = 0xff;
        r[1] = 3;
        let mut t = [0u8; 32];
        t[0] = 0xee;
        let mut miner = [0u8; 20];
        miner.copy_from_slice(&[
            0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45,
            0x67, 0x89, 0xab, 0xcd, 0xef, 0x01,
        ]);
        BlockInfo {
            number: n,
            hash: h,
            parent_hash: p,
            timestamp: 1_700_000_000,
            miner,
            gas_used: 21000,
            gas_limit: 420_000_000,
            difficulty: 1,
            tx_hashes: (0..txs)
                .map(|i| {
                    let mut x = [0u8; 32];
                    x[0] = 0x77;
                    x[1] = i as u8;
                    x
                })
                .collect(),
            tx_root: t,
            state_root: r,
        }
    }
    impl ChainStats for Fake {
        fn head(&self) -> Option<BlockInfo> {
            let mut b = block(self.head.load(SeqCst), 2);
            b.hash[31] = self.salt.load(SeqCst) as u8;
            Some(b)
        }
        fn block(&self, n: u64) -> Option<BlockInfo> {
            (n <= self.head.load(SeqCst)).then(|| block(n, usize::from(n == 1)))
        }
        fn syncing(&self) -> bool {
            self.syncing.load(SeqCst)
        }
        fn gas_price(&self) -> u64 {
            250_000_000
        }
        fn pending_txs(&self) -> usize {
            5
        }
        fn signer(&self) -> Option<[u8; 20]> {
            self.signer
        }
    }
    impl PeerStats for Fake {
        fn peer_count(&self) -> usize {
            self.peers.load(SeqCst) as usize
        }
    }
}
