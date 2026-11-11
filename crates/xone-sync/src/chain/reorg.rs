//! Reorg planning and the committed-block floor. Spec: `spec/chain/reorg-and-set-head.md`.

use alloy_primitives::B256;
use xone_consensus::v2::commit::Link;
use xone_primitives::Header;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReorgError {
    /// geth `errInvalidOldChain` (`core/blockchain.go:126`): a header of the old branch is missing.
    #[error("invalid old chain")]
    InvalidOldChain,
    /// geth `errInvalidNewChain` (`:127`).
    #[error("invalid new chain")]
    InvalidNewChain,
    /// The reorg would drop the committed V2 block `committed` (geth "stop reorg", `:3498`).
    #[error("stop reorg: common ancestor {common} is below committed block {committed} {hash}")]
    BelowCommitted {
        common: u64,
        committed: u64,
        hash: B256,
    },
    /// The committed block is not on the branch it must be on. geth logs "Impossible reorg" and goes
    /// on (`:3504`, `:3513`); a persisted floor makes this a corrupt store, so it is an error here.
    #[error("committed block {committed} {hash} is inconsistent with the canonical chain")]
    FloorInconsistent { committed: u64, hash: B256 },
}

/// Old branch (head first, common ancestor excluded) and new branch (new head first, ancestor
/// excluded) down to their common ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub common: Header,
    pub old: Vec<Header>,
    pub new: Vec<Header>,
}

/// geth `reorg`, `core/blockchain.go:3433-3478`: bring the longer branch to the height of the other,
/// then step both back until the hashes meet. `header` reads any stored header by hash.
pub fn plan(
    header: &dyn Fn(B256) -> Option<Header>,
    old_head: &Header,
    new_head: &Header,
) -> Result<Plan, ReorgError> {
    let parent = |h: &Header, e: ReorgError| -> Result<Header, ReorgError> {
        header(h.parent_hash)
            .filter(|p| p.number + 1 == h.number)
            .ok_or(e)
    };
    let (mut o, mut n) = (old_head.clone(), new_head.clone());
    let (mut old, mut new) = (Vec::new(), Vec::new());
    while o.number > n.number {
        let p = parent(&o, ReorgError::InvalidOldChain)?;
        old.push(std::mem::replace(&mut o, p));
    }
    while n.number > o.number {
        let p = parent(&n, ReorgError::InvalidNewChain)?;
        new.push(std::mem::replace(&mut n, p));
    }
    while o.hash() != n.hash() {
        let (po, pn) = (
            parent(&o, ReorgError::InvalidOldChain)?,
            parent(&n, ReorgError::InvalidNewChain)?,
        );
        old.push(std::mem::replace(&mut o, po));
        new.push(std::mem::replace(&mut n, pn));
    }
    Ok(Plan {
        common: o,
        old,
        new,
    })
}

/// geth `reorg`, `core/blockchain.go:3479-3516` (F-2026-19773, CertiK XDC-08): a reorg whose common
/// ancestor is below the latest committed block, and whose old branch holds that block, is refused.
pub fn check_floor(plan: &Plan, floor: Option<&Link>) -> Result<(), ReorgError> {
    let Some(f) = floor else { return Ok(()) };
    let bad = ReorgError::FloorInconsistent {
        committed: f.number,
        hash: f.hash,
    };
    if plan.common.number < f.number {
        return match plan.old.iter().find(|h| h.number == f.number) {
            Some(h) if h.hash() == f.hash => Err(ReorgError::BelowCommitted {
                common: plan.common.number,
                committed: f.number,
                hash: f.hash,
            }),
            _ => Err(bad),
        };
    }
    if plan.common.number == f.number && plan.common.hash() != f.hash {
        return Err(bad);
    }
    Ok(())
}
