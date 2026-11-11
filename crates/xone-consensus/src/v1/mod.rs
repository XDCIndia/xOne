//! XDPoS V1 as pure functions over explicit inputs: no I/O, no clock, no locks, no global state.
//! A caller (the replay runner, the header verifier) reads headers, state and snapshots and
//! passes them in; an absent input is a typed `Missing*` [`HeaderError`], never an empty list.
//! The rules and their geth-XDC lines are in `spec/consensus/v1-*.md`.
//!
//! * [`snapshot`]: the signer snapshot and its `apply` across blocks;
//! * [`turn`]: in-turn rule, difficulty and the creator checks of `verifySeal`;
//! * [`checkpoint`]: the masternode list of a checkpoint header;
//! * [`penalty`]: the V1 penalty list (comeback and sticky parts are XB-10);
//! * [`double_validation`]: M1 creator and M2 validator;
//! * [`switch`]: the hand-off to V2.

pub mod checkpoint;
pub mod double_validation;
pub mod penalty;
pub mod snapshot;
pub mod switch;
pub mod turn;

pub(crate) mod gosort;

use crate::verify::HeaderError;

/// The checkpoint block of the epoch that holds `number` (geth `getValidatorWithParents`,
/// `consensus/XDPoS/xdpos_api.go:128-129`).
pub fn checkpoint_number(number: u64, epoch: u64) -> Result<u64, HeaderError> {
    number
        .checked_rem(epoch)
        .map(|r| number - r)
        .ok_or(HeaderError::ZeroEpoch)
}

/// geth `verifyCascadingFields`, `consensus/XDPoS/xdpos.go:981`: a block needs at least `period`
/// seconds after its parent. `period` is the genesis `XDPoS.period`; no constant stands in for
/// it (xOneGo #682).
pub fn check_timestamp(parent_time: u64, time: u64, period: u64) -> Result<(), HeaderError> {
    if parent_time.wrapping_add(period) > time {
        return Err(HeaderError::InvalidTimestamp {
            time,
            parent: parent_time,
            period,
        });
    }
    Ok(())
}
