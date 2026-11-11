//! The node's view for the ethstats board (XB-21): [`ChainStats`] over the chain history and the
//! sync progress, [`PeerStats`] over the peer set. Plain reads, no waits beyond the history lock.

use std::sync::{atomic::Ordering::Relaxed, Arc};

use alloy_primitives::keccak256;
use xone_net::peers::Peers;

use super::{chain::Progress, core::Core, hist::Stored, pool::PoolShared};
use crate::observe::{BlockInfo, ChainStats, PeerStats};

/// geth `eth_gasPrice` is the 1559 base fee plus tip; the board shows it as given. ponytail: the head
/// base fee, or 0 before the fee market; a tip oracle comes with the pool's gas-price oracle.
pub struct NodeStats {
    pub core: Arc<Core>,
    pub progress: Arc<Progress>,
    pub pool: Arc<PoolShared>,
    pub peers: Peers,
}

fn info(s: &Stored) -> BlockInfo {
    let h = &s.header;
    BlockInfo {
        number: h.number,
        hash: s.hash.0,
        parent_hash: h.parent_hash.0,
        timestamp: h.timestamp,
        miner: s.miner.0 .0,
        gas_used: h.gas_used,
        gas_limit: h.gas_limit,
        difficulty: u64::try_from(h.difficulty).unwrap_or(u64::MAX),
        tx_hashes: s.body.transactions.iter().map(|t| keccak256(t).0).collect(),
        tx_root: h.tx_root.0,
        state_root: h.state_root.0,
    }
}

impl ChainStats for NodeStats {
    fn head(&self) -> Option<BlockInfo> {
        Some(info(self.core.hist.read().tip()))
    }

    fn block(&self, number: u64) -> Option<BlockInfo> {
        let g = self.core.hist.read();
        let i = usize::try_from(number).ok()?;
        g.blocks.get(i).map(info)
    }

    /// Behind the best peer: the continuation from it has not been seen empty at our head.
    fn syncing(&self) -> bool {
        !self.progress.confirmed.load(Relaxed)
    }

    fn gas_price(&self) -> u64 {
        let g = self.core.hist.read();
        g.tip()
            .header
            .base_fee
            .and_then(|f| u64::try_from(f).ok())
            .unwrap_or(0)
    }

    fn pending_txs(&self) -> usize {
        self.pool.pending_count()
    }

    /// A follower never seals.
    fn signer(&self) -> Option<[u8; 20]> {
        None
    }
}

impl PeerStats for NodeStats {
    fn peer_count(&self) -> usize {
        self.peers.peer_count()
    }
}
