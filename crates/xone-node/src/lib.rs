//! Resource-isolation primitives of ADR-007 (operations limits).
//!
//! Pure, single-owner values. No threads, no I/O, no clock reads, no global
//! state: callers pass time in milliseconds, so every test is deterministic.
#![forbid(unsafe_code)]

mod bucket;
mod disk;
mod peer;
mod queue;

pub use bucket::TokenBucket;
pub use disk::{Action, DiskWatermark};
pub use peer::{Disconnect, Offence, PeerBudget, PeerLimits};
pub use queue::{BoundedQueue, Overflow, Push};

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
