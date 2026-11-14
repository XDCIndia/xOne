//! What the chain manager needs from block execution. The real executor (XB-07/XA-05) implements
//! [`BlockExecutor`]; [`crate::testkit::VectorExecutor`] replays recorded results.

use alloy_primitives::{Bloom, Bytes, B256};
use xone_consensus::verify::Body;
use xone_primitives::Header;
use xone_state::{StateDiff, StateRead};

/// The result of executing one block on its parent's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    /// New values the block writes.
    pub diff: StateDiff,
    /// Consensus-encoded receipts, one per transaction.
    pub receipts: Vec<Bytes>,
    /// State root after applying `diff` on the parent state.
    pub state_root: B256,
    pub gas_used: u64,
    /// Hash of the block access list the execution produced; `Some` from Amsterdam on.
    pub access_list_hash: Option<B256>,
    /// Opaque rows the executor needs to continue after a restart (deployed code, sign
    /// transactions, the V1 snapshot). Persisted in the block's commit ([`crate::store::Record::aux`]);
    /// empty for an executor that keeps nothing.
    pub aux: Bytes,
    /// The receipts root and bloom of `receipts`, when the executor derived them already: the
    /// chain compares the header with these instead of decoding and re-rooting the receipts
    /// (#245). `None` derives them from `receipts`.
    pub roots: Option<ReceiptRoots>,
}

/// What [`Execution::roots`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptRoots {
    pub root: B256,
    pub bloom: Bloom,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("execution failed: {0}")]
    Failed(String),
}

/// Executes `body` on `parent`, the post-state of the block's own parent (canonical or side). It
/// reads no other state and writes nothing.
pub trait BlockExecutor: Sync {
    fn execute(
        &self,
        parent: &dyn StateRead,
        header: &Header,
        body: &Body,
    ) -> Result<Execution, ExecError>;
}
