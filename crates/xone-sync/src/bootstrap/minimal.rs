//! Minimal mode (`spec/bootstrap/minimal-mode.md`): the node starts from the anchor of an imported
//! snapshot, reads no archive, keeps what the derived [`Retention`] says and deletes the rest, and
//! answers for data outside what it holds with the typed
//! [`Missing::UnsupportedHistory`](super::retention::Missing).

use xone_chainspec::ChainSpec;
use xone_db::ManifestState;

use super::retention::{Class, Error, Missing, Retention};
use crate::store::{Store, StoreError};

/// The first block to keep, per segment, for a node whose head is `head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    pub headers_from: u64,
    /// Bodies and receipts: one record, one floor (the smaller of the two classes').
    pub records_from: u64,
}

/// The window of a node at `head` is the window of a pivot at `head` (`Retention::derive`): every
/// rule that can run on the next epoch of blocks finds what it reads. The floors never exceed the
/// committed block: a reorg cannot go below it, and the store keeps what a reorg re-executes.
pub fn keep_from(spec: &ChainSpec, head: u64, committed: Option<u64>) -> Result<Keep, Error> {
    let r = Retention::derive(spec, head)?;
    let cap = committed.unwrap_or(head).min(head);
    let records_from = r.floor(Class::Body).min(r.floor(Class::Receipt)).min(cap);
    let headers_from = r.floor(Class::Header).min(records_from);
    Ok(Keep {
        headers_from,
        records_from,
    })
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MinimalError {
    #[error(transparent)]
    Retention(#[from] Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the datadir has no anchor: it was not bootstrapped from a snapshot")]
    NoAnchor,
    #[error("the datadir has no readable manifest")]
    NoManifest,
    /// `Retention` can only grow without migration (ADR-005 section 12): a datadir that holds less
    /// than this binary derives cannot be started, and is not repaired by reading an archive.
    #[error("{class:?} is held from block {have}, this binary needs it from {need}: import a new snapshot")]
    Shrunk { class: Class, have: u64, need: u64 },
}

fn firsts(store: &impl Store) -> Option<(u64, u64)> {
    match store.scan().manifest {
        ManifestState::Ok(m) => Some((m.entries[0].first_block, m.entries[1].first_block)),
        _ => None,
    }
}

/// Deletes below the floor once the floor has moved at least `slack` blocks past what is held
/// (`slack` 0: at every move). Returns what was kept, when something was deleted.
pub fn prune<S: Store>(
    spec: &ChainSpec,
    store: &mut S,
    committed: Option<u64>,
    slack: u64,
) -> Result<Option<Keep>, MinimalError> {
    let (head, _) = store.head();
    let keep = keep_from(spec, head, committed)?;
    let (h, r) = firsts(store).ok_or(MinimalError::NoManifest)?;
    if keep.headers_from <= h.saturating_add(slack) && keep.records_from <= r.saturating_add(slack)
        || (keep.headers_from <= h && keep.records_from <= r)
    {
        return Ok(None);
    }
    let keep = Keep {
        headers_from: keep.headers_from.max(h),
        records_from: keep.records_from.max(r),
    };
    store.prune(keep.headers_from, keep.records_from)?;
    Ok(Some(keep))
}

/// What a start-up of a minimal node checks: the datadir is anchored, and holds at least what
/// the rules need at its head. A datadir that holds more is accepted.
pub fn check_start(spec: &ChainSpec, store: &impl Store) -> Result<(), MinimalError> {
    store.anchor().ok_or(MinimalError::NoAnchor)?;
    let (head, _) = store.head();
    let (h, r) = firsts(store).ok_or(MinimalError::NoManifest)?;
    let need = Retention::derive(spec, head)?;
    for (class, have, need) in [
        (Class::Header, h, need.floor(Class::Header)),
        (
            Class::Body,
            r,
            need.floor(Class::Body).min(need.floor(Class::Receipt)),
        ),
    ] {
        if have > need.min(head) {
            return Err(MinimalError::Shrunk { class, have, need });
        }
    }
    Ok(())
}

/// What a minimal node holds, for the RPC: the first block of each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub pivot: u64,
    pub headers_from: u64,
    pub records_from: u64,
    pub gaps_from: u64,
    pub state_from: u64,
}

impl Held {
    /// `state_window` is the number of blocks below the head whose state the store opens.
    pub fn of_store(
        spec: &ChainSpec,
        store: &impl Store,
        state_window: u64,
    ) -> Result<Self, MinimalError> {
        let (head, _) = store.head();
        let (headers_from, records_from) = firsts(store).ok_or(MinimalError::NoManifest)?;
        let gaps_from = Retention::derive(spec, head)?.floor(Class::GapSnapshot);
        Ok(Self {
            pivot: head,
            headers_from,
            records_from,
            gaps_from,
            state_from: head.saturating_sub(state_window),
        })
    }

    /// `Ok` when `class` of `block` is held, else the typed refusal, never an empty answer.
    pub fn is_within(&self, class: Class, block: u64) -> Result<(), Missing> {
        let floor = match class {
            Class::Header => self.headers_from,
            Class::Body | Class::Receipt => self.records_from,
            Class::GapSnapshot => self.gaps_from,
            Class::State => self.state_from,
        };
        if block >= floor {
            return Ok(());
        }
        Err(Missing::UnsupportedHistory {
            class,
            block,
            floor,
            pivot: self.pivot,
        })
    }
}
