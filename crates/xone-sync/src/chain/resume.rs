//! Restart recovery. Spec: `spec/chain/resume.md`.
//!
//! The head of a non-empty datadir is read from the committed manifest and checked against the
//! index and the block tables. It is either resumed or refused with a typed error that names the
//! missing record. There is no path that starts at block 0 on a datadir that holds data
//! (xOneGo#720: `headBlock=0`, later "no canonical hash for block #618613", recovered by a wipe).

use alloy_primitives::B256;
use xone_chainspec::ChainSpec;
use xone_db::{reconcile, ManifestState, Recovery, Refuse};

use super::{Chain, HeaderCheck};
use crate::{
    executor::BlockExecutor,
    store::{Store, StoreError},
};

/// Blocks below the head whose index, header and parent links are checked at start-up.
pub const RESUME_WINDOW: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resumed {
    /// New datadir: the caller writes the genesis block.
    Fresh,
    Head {
        number: u64,
        hash: B256,
        /// Segment tails cut by the repair.
        repaired: usize,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResumeError {
    /// `xone_db::reconcile` refused (ADR-005 section 7, rows D2-D10).
    #[error("refused to start: {0:?}")]
    Refused(Refuse),
    #[error("no canonical hash for block #{number}")]
    MissingCanonical { number: u64 },
    #[error("the manifest head {manifest} is not the canonical block {canonical} at #{number}")]
    HeadMismatch {
        number: u64,
        manifest: B256,
        canonical: B256,
    },
    #[error("no header for canonical block #{number} {hash}")]
    MissingHeader { number: u64, hash: B256 },
    #[error("no state for canonical block #{number} {hash}")]
    MissingState { number: u64, hash: B256 },
    #[error("canonical block #{number} does not link to its canonical parent by its parent hash")]
    BrokenLink { number: u64 },
    /// The manifest says new datadir but the tables hold records.
    #[error("the datadir holds records but no manifest")]
    OrphanRecords,
    #[error("store: {0}")]
    Store(StoreError),
}

/// Reconciles the store and checks the head. See the module comment.
pub fn resume<S: Store>(store: &mut S) -> Result<Resumed, ResumeError> {
    let scan = store.scan();
    let (actions, repaired) = match reconcile(scan.generation, &scan.manifest, &scan.tails) {
        Recovery::Fresh if scan.has_records => return Err(ResumeError::OrphanRecords),
        Recovery::Fresh => return Ok(Resumed::Fresh),
        Recovery::Refuse(r) => return Err(ResumeError::Refused(r)),
        Recovery::Ready => (Vec::new(), 0),
        Recovery::Repair(a) => {
            let n = a.len();
            (a, n)
        }
    };
    store.repair(&actions).map_err(ResumeError::Store)?;
    let ManifestState::Ok(m) = &scan.manifest else {
        return Err(ResumeError::Refused(Refuse::ManifestCorrupt));
    };
    let (number, hash) = (m.head_number, m.head_hash);
    match store.canonical_hash(number) {
        None => return Err(ResumeError::MissingCanonical { number }),
        Some(c) if c != hash => {
            return Err(ResumeError::HeadMismatch {
                number,
                manifest: hash,
                canonical: c,
            })
        }
        Some(_) => {}
    }
    let first = m.entries[0].first_block;
    let lowest = number.saturating_sub(RESUME_WINDOW).max(first);
    let mut above: Option<(u64, B256)> = None; // the child whose parent link is checked next
    for n in (lowest..=number).rev() {
        let h = store
            .canonical_hash(n)
            .ok_or(ResumeError::MissingCanonical { number: n })?;
        let header = store
            .header(h)
            .ok_or(ResumeError::MissingHeader { number: n, hash: h })?;
        if let Some((cn, parent)) = above {
            if parent != h {
                return Err(ResumeError::BrokenLink { number: cn });
            }
        }
        above = Some((n, header.parent_hash));
    }
    store
        .state_at(hash)
        .map(|_| ())
        .map_err(|_| ResumeError::MissingState { number, hash })?;
    Ok(Resumed::Head {
        number,
        hash,
        repaired,
    })
}

impl<S: Store, E: BlockExecutor, H: HeaderCheck> Chain<S, E, H> {
    /// Opens a chain on `store`: [`resume`] first, then the manager. Nothing is returned on refusal.
    pub fn open(
        spec: ChainSpec,
        mut store: S,
        exec: E,
        check: H,
    ) -> Result<(Self, Resumed), ResumeError> {
        let resumed = resume(&mut store)?;
        Ok((Self::new(spec, store, exec, check), resumed))
    }
}
