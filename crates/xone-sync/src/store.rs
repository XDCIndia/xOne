//! The storage the chain manager runs on. One trait, implemented by the MDBX binding (later) and by
//! [`crate::testkit::MemStore`]. Every write method is one ADR-005 step with its kill points; a
//! method that returns `Ok` has published its generation (the commit returned).

use alloy_primitives::{Bytes, B256};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_db::{Action, Cause, Durability, ManifestState, SegmentTail};
use xone_primitives::Header;
use xone_state::{CommitError, Missing, StateDiff, StateRead};

/// A block with its hash, computed once.
#[derive(Debug, Clone)]
pub struct Block {
    pub header: Header,
    pub body: Body,
    hash: B256,
}

impl Block {
    pub fn new(header: Header, body: Body) -> Self {
        let hash = header.hash();
        Self { header, body, hash }
    }
    pub fn hash(&self) -> B256 {
        self.hash
    }
    pub fn number(&self) -> u64 {
        self.header.number
    }
    pub fn parent(&self) -> B256 {
        self.header.parent_hash
    }
}

/// An executed block: what a commit or a side insert persists.
#[derive(Debug, Clone)]
pub struct Record {
    pub block: Block,
    pub receipts: Vec<Bytes>,
    pub diff: StateDiff,
}

/// What `unwind_to` does with the blocks above the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unwound {
    /// They stay as side blocks with their state diffs (a reorg may re-adopt them).
    Keep,
    /// They are deleted (`set-head`, geth `delFn`, `blockchain.go:1501-1521`).
    Drop,
}

/// ADR-005 kill points (sections 5 and 8). The names are the IDs of `tests/fault/PLAN.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KillPoint {
    K01,
    K02,
    K03,
    K04,
    K05,
    K06,
    K07,
    K08,
    K09,
    K10,
    K11,
    K12,
    K13,
    K14,
    K15,
    K16,
    K17,
    K18,
    K19,
    U1a,
    U1b,
    U3a,
    U3b,
    U4a,
    U4b,
    U5a,
    U5b,
}

impl KillPoint {
    pub const ALL: [KillPoint; 27] = {
        use KillPoint::*;
        [
            K01, K02, K03, K04, K05, K06, K07, K08, K09, K10, K11, K12, K13, K14, K15, K16, K17,
            K18, K19, U1a, U1b, U3a, U3b, U4a, U4b, U5a, U5b,
        ]
    };
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The process died here (test double). The store accepts no further call.
    #[error("process crashed at {0:?}")]
    Crashed(KillPoint),
    /// ADR-005 section 9: fail-stop. No retry in process.
    #[error("store degraded: {0:?}")]
    Degraded(Cause),
    #[error("state: {0}")]
    State(#[from] CommitError),
    #[error("cannot unwind to {0}: above the head or outside the retained window")]
    Unwind(u64),
}

/// What start-up reads from the datadir before `xone_db::reconcile` decides.
pub struct Scan {
    pub generation: u64,
    pub manifest: ManifestState,
    pub tails: Vec<SegmentTail>,
    /// The index or the block tables hold anything at all.
    pub has_records: bool,
}

pub trait Store {
    // ---- reads. Nothing above the published head is visible on the canonical index.
    fn head(&self) -> (u64, B256);
    fn canonical_hash(&self, number: u64) -> Option<B256>;
    /// Any stored header: canonical, side or stateless.
    fn header(&self, hash: B256) -> Option<Header>;
    /// A block executed and stored with its state diff (canonical or side).
    fn record(&self, hash: B256) -> Option<Record>;
    /// A block stored without state (parent state not available).
    fn stateless(&self, hash: B256) -> Option<Block>;
    /// Post-state of `hash`, canonical inside the retained window or on a side branch.
    fn state_at(&self, hash: B256) -> Result<Box<dyn StateRead + '_>, Missing>;
    /// Highest committed V2 block, persisted with the block commit.
    fn committed(&self) -> Option<Link>;
    fn stateless_children(&self, parent: B256) -> usize;
    fn durability(&self) -> Durability;

    // ---- writes
    /// Extends the head by one block (S1-S7). `committed` is the committed block after this one.
    fn commit_block(&mut self, rec: &Record, committed: Option<Link>) -> Result<(), StoreError>;
    /// Removes every canonical block above `number` in one atomic step (U1-U5): state, receipts,
    /// canonical index, segment tails and consensus rows. The committed block is not changed.
    fn unwind_to(&mut self, number: u64, blocks: Unwound) -> Result<(), StoreError>;
    /// Stores an executed block that is not canonical (one MDBX transaction).
    fn put_side(&mut self, rec: &Record) -> Result<(), StoreError>;
    /// Stores a block whose parent state is not available.
    fn put_stateless(&mut self, block: &Block) -> Result<(), StoreError>;

    // ---- restart
    fn scan(&self) -> Scan;
    /// Applies the actions of `xone_db::reconcile` and the idempotent publication: truncates, removes
    /// orphan chunk files and a stale journal.
    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError>;
}
