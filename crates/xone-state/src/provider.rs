//! `StateProvider`: opens the post-state of a block as a read-only [`StateView`].
//!
//! The flat tables hold the head. The post-state of an older retained block is the flat value,
//! except where a later block's changeset holds a previous value: the lowest later changeset that
//! touches a key has the value as it was right after the block being opened. No replay, no copy.
//! A block on a side branch is a parent view plus the diff of the side blocks above it
//! ([`StateView::child`]); nothing is written to the flat tables for it.
//!
//! ponytail: a read scans the changesets above the opened block (at most `retention`), and the
//! side index and the changesets are unbounded or in memory. An index per key and a persisted side
//! index come with the MDBX binding, together with the open-latency target.

use crate::{
    changeset::{ChangeSet, StateDiff},
    flat::{FlatRead, FlatWrite},
    mpt::{StateTrie, Undo},
    Account,
};
use alloy_primitives::{keccak256, Address, B256, U256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Mutex, MutexGuard, PoisonError},
};
use xone_primitives::WideBalanceError;

/// A state that cannot be opened: unknown hash, or outside the retained window.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Missing {
    #[error("no state for block hash {0}")]
    Block(B256),
    #[error("block number {0} is outside the retained window")]
    Number(u64),
    /// The state is a view that cannot compute a root (see [`StateRead::root_with`]).
    #[error("this state cannot compute a state root")]
    NoRoot,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommitError {
    #[error("parent {parent} is not the head {head}")]
    NotExtending { head: B256, parent: B256 },
    #[error("block {0} is already known")]
    Known(B256),
    #[error(transparent)]
    Missing(#[from] Missing),
    #[error(transparent)]
    Balance(#[from] WideBalanceError),
}

/// Reads of one state. There is no mutating method: this is the only thing the executor's
/// prewarm and parallel readers receive.
pub trait StateRead {
    fn account(&self, address: Address) -> Option<Account>;
    /// Zero when absent.
    fn storage(&self, address: Address, slot: B256) -> U256;
    /// The state root after `diff` on top of this state. Only the head view of a provider can
    /// compute it (the `StateRead` impl of [`StateView`]); any other state is [`Missing::NoRoot`].
    fn root_with(&self, _diff: &StateDiff) -> Result<B256, Missing> {
        Err(Missing::NoRoot)
    }
}

/// The head's incremental trie, and the diff whose root was asked for but that is not committed:
/// the trie already holds it, with what it overwrote, so the commit of the same diff costs nothing.
struct Head {
    trie: StateTrie,
    pending: Option<(StateDiff, B256, Undo)>,
}

impl Head {
    /// The trie of the committed head.
    fn settle(&mut self) {
        if let Some((_, _, undo)) = self.pending.take() {
            self.trie.undo(undo);
        }
    }

    fn root_with(&mut self, diff: &StateDiff) -> B256 {
        if let Some((d, root, _)) = &self.pending {
            if d == diff {
                return *root;
            }
        }
        self.settle();
        let h = diff.hashed();
        let (root, undo) = self.trie.apply(&h.accounts, &h.storage);
        self.pending = Some((diff.clone(), root, undo));
        root
    }
}

pub struct StateProvider<F> {
    flat: F,
    head: Mutex<Head>,
    retention: u64,
    /// Oldest openable block.
    base: (u64, B256),
    /// Blocks above `base`, each with the changeset that undoes it.
    chain: BTreeMap<u64, (B256, ChangeSet)>,
    by_hash: HashMap<B256, u64>,
    /// Side blocks: hash to (parent hash, diff).
    side: HashMap<B256, (B256, StateDiff)>,
}

impl<F> StateProvider<F> {
    pub fn head(&self) -> (u64, B256) {
        self.chain
            .last_key_value()
            .map_or(self.base, |(n, (h, _))| (*n, *h))
    }

    fn head_trie(&self) -> MutexGuard<'_, Head> {
        self.head.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn root_with(&self, diff: &StateDiff) -> B256 {
        self.head_trie().root_with(diff)
    }
}

impl<F: FlatWrite> StateProvider<F> {
    /// `flat` is the post-state of block (`number`, `hash`). `retention` is the number of blocks
    /// above the oldest openable one that keep a changeset.
    pub fn new(flat: F, number: u64, hash: B256, retention: u64) -> Self {
        Self::with_opaque(flat, number, hash, retention, &BTreeMap::new())
    }

    /// [`Self::new`] over a pruned state: the accounts in `opaque` have no slots in `flat`, only a
    /// storage root. For tests that commit a window of a large state.
    pub fn with_opaque(
        flat: F,
        number: u64,
        hash: B256,
        retention: u64,
        opaque: &BTreeMap<Address, B256>,
    ) -> Self {
        let trie = StateTrie::from_flat_pruned(&flat, opaque);
        Self {
            flat,
            head: Mutex::new(Head {
                trie,
                pending: None,
            }),
            retention,
            base: (number, hash),
            chain: BTreeMap::new(),
            by_hash: HashMap::from([(hash, number)]),
            side: HashMap::new(),
        }
    }

    /// The head state, read-only. For the reference root (`crate::trie::flat_root`) and tests.
    pub fn flat(&self) -> &F {
        &self.flat
    }

    /// The state root of the head, from the incremental trie.
    pub fn root(&self) -> B256 {
        let mut h = self.head_trie();
        h.settle();
        h.trie.root()
    }

    /// The state of a datadir whose tables hold the head: `flat` reads them, `chain` lists the
    /// blocks above `base` (oldest first) with the changeset each one wrote. No replay: the
    /// changesets are the ones the store persisted.
    pub fn from_head(
        flat: F,
        base: (u64, B256),
        chain: Vec<(u64, B256, ChangeSet)>,
        retention: u64,
    ) -> Self {
        let trie = StateTrie::from_flat(&flat);
        let by_hash = std::iter::once((base.1, base.0))
            .chain(chain.iter().map(|(n, h, _)| (*h, *n)))
            .collect();
        Self {
            flat,
            head: Mutex::new(Head {
                trie,
                pending: None,
            }),
            retention,
            base,
            chain: chain.into_iter().map(|(n, h, cs)| (n, (h, cs))).collect(),
            by_hash,
            side: HashMap::new(),
        }
    }

    fn can_extend(&self, hash: B256, parent: B256) -> Result<u64, CommitError> {
        let (number, head) = self.head();
        if parent != head {
            return Err(CommitError::NotExtending { head, parent });
        }
        if self.at(hash).is_ok() {
            return Err(CommitError::Known(hash));
        }
        Ok(number)
    }

    /// The root after `diff` on top of the head, without committing it.
    /// Writes the next canonical block and keeps its changeset (one commit).
    pub fn commit_block(
        &mut self,
        hash: B256,
        parent: B256,
        diff: &StateDiff,
    ) -> Result<(), CommitError> {
        let number = self.can_extend(hash, parent)?;
        let cs = diff.apply(&mut self.flat)?;
        self.advance(number, hash, diff, cs);
        Ok(())
    }

    /// [`Self::commit_block`] for a block whose rows and changeset `cs` are already in the tables
    /// behind the flat state (the store commits first): the flat state is brought in line, nothing
    /// is read from it. Fails, before any change, on a balance above the legal bound.
    pub fn commit_written(
        &mut self,
        hash: B256,
        parent: B256,
        diff: &StateDiff,
        cs: ChangeSet,
    ) -> Result<(), CommitError> {
        let number = self.can_extend(hash, parent)?;
        for a in diff.accounts.values().flatten() {
            a.balance.check_legal()?;
        }
        self.flat.commit_written(&diff.hashed(), &cs);
        self.advance(number, hash, diff, cs);
        Ok(())
    }

    fn advance(&mut self, number: u64, hash: B256, diff: &StateDiff, cs: ChangeSet) {
        let h = self.head.get_mut().unwrap_or_else(PoisonError::into_inner);
        match h.pending.take() {
            Some((d, _, _)) if d == *diff => {}
            pending => {
                if let Some((_, _, undo)) = pending {
                    h.trie.undo(undo);
                }
                let hd = diff.hashed();
                h.trie.apply(&hd.accounts, &hd.storage);
            }
        }
        self.chain.insert(number + 1, (hash, cs));
        self.by_hash.insert(hash, number + 1);
        while u64::try_from(self.chain.len()).unwrap_or(u64::MAX) > self.retention {
            if let Some((n, (h, _))) = self.chain.pop_first() {
                self.by_hash.remove(&self.base.1);
                self.base = (n, h);
            }
        }
    }

    /// Reverts every block above `number`. The head moves to `number`.
    pub fn unwind_to(&mut self, number: u64) -> Result<(), Missing> {
        if number < self.base.0 || number > self.head().0 {
            return Err(Missing::Number(number));
        }
        let h = self.head.get_mut().unwrap_or_else(PoisonError::into_inner);
        h.settle();
        while let Some((n, (hash, cs))) = self.chain.pop_last() {
            if n <= number {
                self.chain.insert(n, (hash, cs));
                break;
            }
            cs.revert(&mut self.flat);
            h.trie.apply(&cs.accounts, &cs.storage);
            self.by_hash.remove(&hash);
        }
        Ok(())
    }

    /// Records a block that is not canonical, on top of a known parent (canonical or side).
    pub fn insert_side(
        &mut self,
        hash: B256,
        parent: B256,
        diff: StateDiff,
    ) -> Result<(), CommitError> {
        if self.at(hash).is_ok() {
            return Err(CommitError::Known(hash));
        }
        self.at(parent)?;
        for a in diff.accounts.values().flatten() {
            a.balance.check_legal()?;
        }
        self.side.insert(hash, (parent, diff));
        Ok(())
    }

    /// The accounts and the non-zero slots of the post-state of canonical block `hash`, by hashed
    /// key: the head tables, and the keys that a later changeset touches (an account deleted after
    /// the block is only there). Any order. A side block is [`Missing::Block`].
    /// ponytail: whole state in memory, the MDBX binding streams cursors.
    #[allow(clippy::type_complexity)]
    pub fn rows_at(
        &self,
        hash: B256,
    ) -> Result<(Vec<(B256, Account)>, Vec<(B256, B256, U256)>), Missing> {
        let view = self.at(hash)?;
        if !view.overlays.is_empty() {
            return Err(Missing::Block(hash));
        }
        let mut addrs: std::collections::BTreeSet<B256> =
            self.flat.hashed_accounts().map(|(a, _)| a).collect();
        let mut slots: std::collections::BTreeSet<(B256, B256)> = Default::default();
        for (_, cs) in self
            .chain
            .range(view.number + 1..)
            .map(|(n, (_, c))| (n, c))
        {
            addrs.extend(cs.accounts.keys());
            slots.extend(cs.storage.keys());
        }
        let accounts: Vec<_> = addrs
            .into_iter()
            .filter_map(|a| view.hashed_account(a).map(|x| (a, x)))
            .collect();
        for (a, _) in &accounts {
            slots.extend(self.flat.hashed_slots(*a).map(|(s, _)| (*a, s)));
        }
        let live: std::collections::BTreeSet<B256> = accounts.iter().map(|(a, _)| *a).collect();
        let storage = slots
            .into_iter()
            .filter(|(a, _)| live.contains(a))
            .filter_map(|(a, s)| {
                let v = view.hashed_storage(a, s);
                (!v.is_zero()).then_some((a, s, v))
            })
            .collect();
        Ok((accounts, storage))
    }

    /// The post-state of `hash`, canonical (inside the retained window) or side.
    pub fn at(&self, hash: B256) -> Result<StateView<'_, F>, Missing> {
        let mut overlays = Vec::new();
        let mut cur = hash;
        loop {
            if let Some(&number) = self.by_hash.get(&cur) {
                return Ok(StateView {
                    provider: self,
                    number,
                    overlays,
                });
            }
            let (parent, diff) = self.side.get(&cur).ok_or(Missing::Block(cur))?;
            overlays.push(diff); // newest first
            cur = *parent;
        }
    }
}

/// A read-only state. It borrows the provider, so the provider cannot commit or unwind while a
/// view is alive, and it has no method that writes:
///
/// ```compile_fail,E0502
/// use alloy_primitives::B256;
/// use xone_state::{MemFlat, StateDiff, StateProvider};
/// let mut p = StateProvider::new(MemFlat::default(), 0, B256::ZERO, 8);
/// let view = p.at(B256::ZERO).unwrap();
/// p.commit_block(B256::repeat_byte(1), B256::ZERO, &StateDiff::default()).unwrap();
/// let _ = view.number();
/// ```
///
/// ```compile_fail,E0599
/// use alloy_primitives::{Address, B256};
/// use xone_state::{MemFlat, StateProvider};
/// let p = StateProvider::new(MemFlat::default(), 0, B256::ZERO, 8);
/// let mut view = p.at(B256::ZERO).unwrap();
/// view.set_account(Address::ZERO, None);
/// ```
pub struct StateView<'a, F> {
    provider: &'a StateProvider<F>,
    number: u64,
    /// Side diffs above the canonical block `number`, newest first.
    overlays: Vec<&'a StateDiff>,
}

impl<'a, F> StateView<'a, F> {
    /// Number of the canonical block this view stands on (its side ancestors not counted).
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The state after applying `diff` on top of this one. `self` is consumed, not changed.
    pub fn child(mut self, diff: &'a StateDiff) -> Self {
        self.overlays.insert(0, diff);
        self
    }
}

impl<F: FlatRead> StateRead for StateView<'_, F> {
    /// The state root after `diff` on top of this view, from the head's incremental trie: only the
    /// paths `diff` touches are hashed again, and the committed state is not changed. A commit of the
    /// same diff keeps the work. Only the head view has its trie at hand: a view with side overlays
    /// or an older canonical block is [`Missing::Number`].
    fn root_with(&self, diff: &StateDiff) -> Result<B256, Missing> {
        if !self.overlays.is_empty() || self.number != self.provider.head().0 {
            return Err(Missing::Number(self.number));
        }
        Ok(self.provider.root_with(diff))
    }

    fn account(&self, address: Address) -> Option<Account> {
        if let Some(a) = self.overlays.iter().find_map(|d| d.accounts.get(&address)) {
            return a.clone();
        }
        self.hashed_account(keccak256(address))
    }

    fn storage(&self, address: Address, slot: B256) -> U256 {
        for d in &self.overlays {
            if matches!(d.accounts.get(&address), Some(None)) {
                return U256::ZERO;
            }
            if let Some(v) = d.storage.get(&(address, slot)) {
                return *v;
            }
        }
        self.hashed_storage(keccak256(address), keccak256(slot))
    }
}

impl<F: FlatRead> StateView<'_, F> {
    /// The canonical state at `self.number`, by hashed key (side overlays not consulted).
    fn hashed_account(&self, ha: B256) -> Option<Account> {
        for (_, (_, cs)) in self.provider.chain.range(self.number + 1..) {
            if let Some(old) = cs.accounts.get(&ha) {
                return old.clone();
            }
        }
        self.provider.flat.hashed_account(ha)
    }

    fn hashed_storage(&self, ha: B256, hs: B256) -> U256 {
        for (_, (_, cs)) in self.provider.chain.range(self.number + 1..) {
            if let Some(old) = cs.storage.get(&(ha, hs)) {
                return *old;
            }
        }
        self.provider.flat.hashed_storage(ha, hs)
    }
}
