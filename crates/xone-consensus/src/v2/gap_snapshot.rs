//! `SnapshotV2`: the candidate pool of an epoch, fixed at its gap block.
//!
//! A node that executes the gap block reads the candidates of the 0x88 contract from the state of
//! that block, with their caps, drops the zero address and sorts by cap, largest first, with the
//! frozen quicksort, equal caps in the order that sort leaves them (geth `UpdateMasternodesFromHeader`
//! and `sortCandidatesByStake`, `engine_v2/engine.go:2277`, `:2169`). The pool is stored whole: the
//! cap on the masternode count and the penalties are applied later, at the switch
//! ([`super::masternodes`]). The first V2 epoch has no V2 gap block; its pool is the list in the
//! `extraData` of the V1 switch block (`getSnapshot`, `engine.go:1379-1416`).

use alloy_primitives::{Address, U256};
use xone_state::{system_contracts::validator_0x88, StateRead};

use super::Error;
use crate::v1::{checkpoint::masternodes_of, gosort::sort_slice};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotV2 {
    /// Number of the gap block.
    pub number: u64,
    /// geth `NextEpochCandidates`.
    pub candidates: Vec<Address>,
}

/// The gap block of the epoch that holds `number`: `number - number % epoch - gap`, or 0 when the
/// epoch start is not above the gap (`engine.go:924-927`, `:3357-3361`).
pub fn gap_number(number: u64, epoch: u64, gap: u64) -> Result<u64, Error> {
    let base = number.checked_sub(number.checked_rem(epoch).ok_or(Error::ZeroEpoch)?);
    Ok(base.filter(|b| *b > gap).map_or(0, |b| b - gap))
}

/// Whether the pool of the epoch that holds `number` is the V1 list: its epoch starts at the V2 switch
/// block, so its gap block is a V1 block (`getSnapshot`, `engine.go:931`, `:947` cache, `:1110` DB, `:1379-1416` rebuild). This covers every
/// switch of the first V2 epoch, not only the block after the switch block.
pub fn uses_v1_list(number: u64, epoch: u64, switch_block: u64) -> Result<bool, Error> {
    let r = number.checked_rem(epoch).ok_or(Error::ZeroEpoch)?;
    Ok(number - r == switch_block)
}

/// The pool of the gap block `number` from its post-state. No candidate is [`Error::Missing`]
/// (geth then looks for another source; here the caller decides).
pub fn build<S: StateRead>(state: &S, number: u64) -> Result<SnapshotV2, Error> {
    let mut pool: Vec<(Address, U256)> = validator_0x88::candidates(state)
        .into_iter()
        .filter(|a| !a.is_zero())
        .map(|a| (a, validator_0x88::candidate_cap(state, a)))
        .collect();
    if pool.is_empty() {
        return Err(Error::Missing {
            what: "0x88 candidates",
            number,
        });
    }
    sort_slice(&mut pool, |a, b| a.1 >= b.1);
    Ok(SnapshotV2 {
        number,
        candidates: pool.into_iter().map(|(a, _)| a).collect(),
    })
}

/// The pool of the first V2 epoch: the masternode list of the V1 switch block's `extraData`, in
/// that order.
pub fn bootstrap(switch_extra: &[u8], switch_block: u64) -> Result<SnapshotV2, Error> {
    let candidates = masternodes_of(switch_extra).map_err(|_| Error::Missing {
        what: "V1 switch block extra data",
        number: switch_block,
    })?;
    if candidates.is_empty() {
        return Err(Error::Missing {
            what: "V1 switch block masternodes",
            number: switch_block,
        });
    }
    Ok(SnapshotV2 {
        number: switch_block,
        candidates,
    })
}

/// geth `GetStandbynodes` (`engine.go:3351-3375`): the pool minus the masternodes and the penalties
/// of the epoch, in pool order. Empty when there are as many masternodes as candidates.
pub fn standby(pool: &[Address], masternodes: &[Address], penalties: &[Address]) -> Vec<Address> {
    if masternodes.len() == pool.len() {
        return Vec::new();
    }
    pool.iter()
        .filter(|a| !masternodes.contains(a) && !penalties.contains(a))
        .copied()
        .collect()
}
