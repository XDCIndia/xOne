//! The ethstats board while `--syncmode snap` runs, before there is a chain to read: `snapsync` puts
//! its peer set and the best header it has seen here; [`Snap`] answers the board's reads from them.
//! ponytail: process-wide, one snap sync per process.

use std::sync::Mutex;

use xone_net::peers::Peers;
use xone_primitives::Header;

use crate::observe::{BlockInfo, ChainStats, PeerStats};

static PEERS: Mutex<Option<Peers>> = Mutex::new(None);
static HEAD: Mutex<Option<BlockInfo>> = Mutex::new(None);

pub fn set_peers(p: &Peers) {
    *PEERS.lock().unwrap() = Some(p.clone());
}

/// The best header seen so far: what the board shows as the head while syncing.
pub fn set_head(h: &Header) {
    *HEAD.lock().unwrap() = Some(BlockInfo {
        number: h.number,
        hash: h.hash().0,
        parent_hash: h.parent_hash.0,
        timestamp: h.timestamp,
        miner: [0; 20],
        gas_used: h.gas_used,
        gas_limit: h.gas_limit,
        difficulty: u64::try_from(h.difficulty).unwrap_or(u64::MAX),
        tx_hashes: vec![],
        tx_root: h.tx_root.0,
        state_root: h.state_root.0,
    });
}

pub struct Snap;

impl ChainStats for Snap {
    fn head(&self) -> Option<BlockInfo> {
        HEAD.lock().unwrap().clone()
    }
    fn block(&self, number: u64) -> Option<BlockInfo> {
        self.head().filter(|b| b.number == number)
    }
    fn syncing(&self) -> bool {
        true
    }
    fn gas_price(&self) -> u64 {
        0
    }
    fn pending_txs(&self) -> usize {
        0
    }
    fn signer(&self) -> Option<[u8; 20]> {
        None
    }
}

impl PeerStats for Snap {
    fn peer_count(&self) -> usize {
        PEERS.lock().unwrap().as_ref().map_or(0, Peers::peer_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_is_the_best_header_and_syncing_is_true() {
        assert!(Snap.syncing());
        assert_eq!(Snap.peer_count(), 0);
        let h = Header {
            number: 7,
            ..Default::default()
        };
        set_head(&h);
        assert_eq!(Snap.head().unwrap().number, 7);
        assert_eq!(Snap.head().unwrap().hash, h.hash().0);
    }
}
