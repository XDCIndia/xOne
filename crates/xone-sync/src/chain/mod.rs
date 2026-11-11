//! The chain manager. [`Chain`] owns the store behind two locks:
//!
//! * `import`, a mutex that serialises writers (insert, reorg, set-head);
//! * `store`, a read-write lock held only for the length of one store call.
//!
//! Consensus reads (header verification and everything it derives: epoch context, gap snapshot,
//! penalties) run before the import lock is taken, through a [`Reader`] that locks per call. A
//! consensus read therefore never waits for the import lock and the import lock never waits for a
//! consensus read (xOneGo PR 697: one lock across verification and execution deadlocked at the first
//! cold epoch switch). Spec: `spec/chain/insert.md`.

use alloy_primitives::B256;
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use xone_chainspec::ChainSpec;
use xone_consensus::{v2::commit::Link, verify::HeaderError};
use xone_primitives::Header;
use xone_state::{Missing, StateRead};

use crate::{executor::BlockExecutor, store::Store};

pub mod fork_choice;
pub mod insert;
pub mod reorg;
pub mod resume;
pub mod set_head;
pub mod side_index;

pub use insert::{InsertError, Outcome};
pub use reorg::ReorgError;
pub use resume::{ResumeError, Resumed};
pub use set_head::SetHeadError;

/// Read-only view of the chain for consensus code. Every call takes the store lock for its own
/// duration and releases it before returning, so a caller may take as long as it likes between calls.
pub trait ChainRead {
    fn header(&self, hash: B256) -> Option<Header>;
    fn canonical_hash(&self, number: u64) -> Option<B256>;
    /// Runs `f` on the post-state of `hash`. The state is borrowed from the store, so it cannot
    /// leave the closure; the lock is released when the closure returns.
    fn with_state(&self, hash: B256, f: &mut dyn FnMut(&dyn StateRead)) -> Result<(), Missing>;
    fn committed(&self) -> Option<Link>;
}

/// Header verification, with everything it reads. Called with no import lock held. The production
/// implementation builds the `Context` of `xone_consensus::verify` from `chain` and calls
/// `HeaderVerifier`; `now` is the caller's clock in seconds.
pub trait HeaderCheck: Sync {
    fn check(
        &self,
        chain: &dyn ChainRead,
        header: &Header,
        parent: &Header,
        now: u64,
    ) -> Result<(), HeaderError>;

    /// Called after `set-head` (geth `InvalidateCaches`, `engine_v2/engine.go:3455`): drop any cache
    /// keyed by a block above `new_head`.
    fn invalidate(&self, _new_head: u64) {}
}

impl<F> HeaderCheck for F
where
    F: Fn(&dyn ChainRead, &Header, &Header, u64) -> Result<(), HeaderError> + Sync,
{
    fn check(
        &self,
        chain: &dyn ChainRead,
        header: &Header,
        parent: &Header,
        now: u64,
    ) -> Result<(), HeaderError> {
        self(chain, header, parent, now)
    }
}

pub struct Chain<S, E, H> {
    pub(crate) spec: ChainSpec,
    pub(crate) exec: E,
    pub(crate) check: H,
    store: RwLock<S>,
    import: Mutex<()>,
}

/// [`ChainRead`] over a [`Chain`]'s store lock.
pub struct Reader<'a, S>(&'a RwLock<S>);

impl<S: Store> ChainRead for Reader<'_, S> {
    fn header(&self, hash: B256) -> Option<Header> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .header(hash)
    }
    fn canonical_hash(&self, number: u64) -> Option<B256> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .canonical_hash(number)
    }
    fn with_state(&self, hash: B256, f: &mut dyn FnMut(&dyn StateRead)) -> Result<(), Missing> {
        let g = self.0.read().unwrap_or_else(PoisonError::into_inner);
        let view = g.state_at(hash)?;
        f(&*view);
        Ok(())
    }
    fn committed(&self) -> Option<Link> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .committed()
    }
}

impl<S: Store, E: BlockExecutor, H: HeaderCheck> Chain<S, E, H> {
    pub fn new(spec: ChainSpec, store: S, exec: E, check: H) -> Self {
        Self {
            spec,
            exec,
            check,
            store: RwLock::new(store),
            import: Mutex::new(()),
        }
    }

    pub fn reader(&self) -> Reader<'_, S> {
        Reader(&self.store)
    }

    pub fn head(&self) -> (u64, B256) {
        self.rd().head()
    }

    pub fn committed(&self) -> Option<Link> {
        self.rd().committed()
    }

    /// True when the V2 rules apply to `number` (geth `keepsV2Head`: above the switch block).
    pub fn is_v2(&self, number: u64) -> bool {
        self.spec
            .xdpos
            .v2
            .as_ref()
            .is_some_and(|v| number > v.switch_block)
    }

    pub fn into_store(self) -> S {
        self.store
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn rd(&self) -> RwLockReadGuard<'_, S> {
        self.store.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn wr(&self) -> RwLockWriteGuard<'_, S> {
        self.store.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn lock_import(&self) -> MutexGuard<'_, ()> {
        self.import.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
