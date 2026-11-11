//! The staged pipeline (XB-14): headers, bodies, senders, execution, finish. One durable
//! checkpoint per stage lives in the [`Store`] and is written in one transaction with the rows the
//! stage produced (ADR-005 S5, S6). Every stage starts at its own checkpoint, so a restart repeats
//! at most the batch that was in flight. Specification: `spec/sync/stages.md`.
//!
//! The pipeline is pull-driven: a stage never runs ahead of the stage before it by more than its
//! bound (ADR-007 Q8 to Q11), so the producer waits by not being called, and memory does not grow
//! with the chain. Execution is `Chain::insert`, the same path as live import: header check, body
//! check, execution on the parent's state, receipts, gas, access list and state root, then the
//! commit. There is no other way to advance the head.

use crate::{
    chain::{Chain, HeaderCheck, InsertError, Outcome, SetHeadError},
    executor::BlockExecutor,
    store::{Stage, Store, StoreError},
};
use xone_consensus::verify::{Body, BodyError};
use xone_executor::TxError;
use xone_primitives::Header;

pub mod bodies;
pub mod execute;
pub mod finish;
pub mod headers;
pub mod senders;

/// ADR-007 Q8: staged headers waiting for execution.
pub const Q8_HEADERS: usize = 2_048;
/// ADR-007 Q9: blocks per body batch, and the bytes of one batch.
pub const Q9_BLOCKS: usize = 128;
pub const Q9_BYTES: usize = 64 << 20;
/// ADR-007 Q10: blocks handed to execution at a time. Q11 (two commits) holds by construction:
/// `Chain::insert` commits one block before it returns.
pub const Q10_BLOCKS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub headers: usize,
    pub body_blocks: usize,
    pub body_bytes: usize,
    pub exec_blocks: usize,
    /// Threads of the senders stage.
    pub workers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            headers: Q8_HEADERS,
            body_blocks: Q9_BLOCKS,
            body_bytes: Q9_BYTES,
            exec_blocks: Q10_BLOCKS,
            workers: 4,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("source: {0}")]
pub struct SourceError(pub String);

/// Where blocks come from: a file, or peers.
pub trait Source {
    /// Up to `max` consecutive headers from number `from` on. Empty: the source has no block `from`.
    fn headers(&mut self, from: u64, max: usize) -> Result<Vec<Header>, SourceError>;
    /// The bodies of `headers`, in order. It may return a prefix: it stops once `max_bytes` of
    /// transactions are collected (always at least one body), so no body is fetched twice. None at
    /// all is a stall.
    fn bodies(&mut self, headers: &[Header], max_bytes: usize) -> Result<Vec<Body>, SourceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("block {number}: {error}")]
    Insert { number: u64, error: InsertError },
    /// The headers do not form a chain: the source's answer, or the rows staged before a restart.
    #[error("block {number} does not link to its parent (unwind the stages to restart)")]
    Link { number: u64 },
    #[error("block {number}: body does not match the header: {error}")]
    Body { number: u64, error: BodyError },
    #[error("block {number}, transaction {index}: {error}")]
    Sender {
        number: u64,
        index: usize,
        error: TxError,
    },
    /// The source could not serve a block the stage needs (a typed stall, not a wait).
    #[error("no {stage:?} data for block {number} from the source")]
    Stall { stage: Stage, number: u64 },
    #[error("staged row of block {number} is missing")]
    Missing { number: u64 },
    #[error("block {number} extended nothing: {outcome:?}")]
    Unexpected { number: u64, outcome: Outcome },
    #[error("set-head: {0}")]
    SetHead(#[from] SetHeadError),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub headers: u64,
    pub executed: u64,
}

/// Runs the stages in rounds until the source has no more blocks. `now` is the clock the header
/// check reads, in seconds.
pub fn sync<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    src: &mut impl Source,
    lim: &Limits,
    now: u64,
) -> Result<Summary, SyncError> {
    let mut sum = Summary::default();
    loop {
        let h = headers::run(chain, src, lim)?;
        bodies::run(chain, src, lim)?;
        senders::run(chain, lim)?;
        let e = execute::run(chain, lim, now)?;
        finish::run(chain)?;
        sum.headers += h;
        sum.executed += e;
        if h == 0 && e == 0 {
            return Ok(sum);
        }
    }
}

/// Reorg or `set-head` below the head: the staged rows and the checkpoints above `to` go first,
/// then the chain manager unwinds the head. A crash between the two leaves the stages cut and
/// the head where it was, which the next run repeats.
pub fn unwind<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    to: u64,
) -> Result<(), SyncError> {
    chain.wr().unwind_stages(to)?;
    chain.set_head(to)?;
    Ok(())
}

/// `from..=upto` is empty; both are block numbers.
pub(crate) fn span(ckpt: u64, head: u64, upto: u64, limit: usize) -> Option<(u64, u64)> {
    let from = ckpt.max(head) + 1;
    (from <= upto).then(|| {
        let n = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(upto - from + 1);
        (from, n)
    })
}
