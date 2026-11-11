//! Resource-isolation primitives of ADR-007 (operations limits), plus the node config,
//! datadir marker and bounded lifecycle of XB-19.
//!
//! The primitives are pure, single-owner values: no threads, no I/O, no clock reads,
//! no global state; callers pass time in milliseconds. `config`, `datadir`, `enode`
//! and `lifecycle` do I/O and own threads.
#![forbid(unsafe_code)]

pub mod config;
pub mod datadir;
mod disk;
pub mod enode;
pub mod lifecycle;
pub mod services;

pub use config::{Config, ConfigError, Profile};
pub use disk::{Action, DiskWatermark};
pub use lifecycle::{Node, NodeError, Service, Shutdown};
pub use xone_primitives::limits::{
    BoundedQueue, Disconnect, Offence, Overflow, PeerBudget, PeerLimits, Push, TokenBucket,
};

/// Scheduling class (ADR-007 section 3). Lower value runs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Round timer and vote path.
    P0Consensus,
    /// Block verification and durable commit.
    P1Verify,
    /// Sync.
    P2Sync,
    /// RPC traces and snapshot serving.
    P3Serve,
    /// Pruning.
    P4Prune,
}

#[cfg(test)]
mod tests {
    use super::Priority::*;

    #[test]
    fn consensus_orders_before_everything() {
        assert!(
            P0Consensus < P1Verify && P1Verify < P2Sync && P2Sync < P3Serve && P3Serve < P4Prune
        );
    }
}
