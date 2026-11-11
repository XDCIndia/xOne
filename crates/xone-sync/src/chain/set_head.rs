//! `set-head N`. Spec: `spec/chain/reorg-and-set-head.md`.

use super::{Chain, HeaderCheck};
use crate::{
    executor::BlockExecutor,
    store::{Store, StoreError, Unwound},
};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SetHeadError {
    #[error("target {target} is above the head {head}")]
    AboveHead { target: u64, head: u64 },
    /// `set-head` does not revert a committed V2 block.
    #[error("target {target} is below the committed block {committed}")]
    BelowCommitted { target: u64, committed: u64 },
    /// geth rewinds to the nearest block that has state; this store refuses instead.
    #[error("no state is retained at block {0}")]
    NotRetained(u64),
    #[error("store: {0}")]
    Store(StoreError),
}

impl<S: Store, E: BlockExecutor, H: HeaderCheck> Chain<S, E, H> {
    /// Rewinds the chain to block `target` (geth `SetHead`, `core/blockchain.go:1194`). State,
    /// receipts, canonical index and consensus tables go in one atomic step. The blocks above are
    /// deleted (`delFn`, `:1501-1521`), so importing them again executes them again. The consensus caches
    /// are then dropped.
    pub fn set_head(&self, target: u64) -> Result<(), SetHeadError> {
        let _import = self.lock_import();
        let head = self.rd().head().0;
        if target > head {
            return Err(SetHeadError::AboveHead { target, head });
        }
        if target == head {
            return Ok(());
        }
        if let Some(f) = self.rd().committed().filter(|f| target < f.number) {
            return Err(SetHeadError::BelowCommitted {
                target,
                committed: f.number,
            });
        }
        self.wr()
            .unwind_to(target, Unwound::Drop)
            .map_err(|e| match e {
                StoreError::Unwind(n) => SetHeadError::NotRetained(n),
                e => SetHeadError::Store(e),
            })?;
        self.check.invalidate(target);
        Ok(())
    }
}
