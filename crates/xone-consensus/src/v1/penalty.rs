//! The V1 penalty list of a checkpoint: masternodes of the previous epoch that did not sign
//! enough. geth picks the rule by block number (`v1CheckpointPenalties`, `xdpos.go:1215-1231`).
//!
//! * before `TIPSigning`: `HookPenalty` (`eth/hooks/hooks.go:124-176`): the previous epoch's
//!   masternodes that signed none of the blocks `i` in `[number - epoch, number)` with
//!   `i % MergeSignRange == 0` or not yet `TIP2019`, counted in the 0x89 state;
//! * from `TIPSigning`: `HookPenaltyTIPSigning` (`hooks.go:182-302`): the previous epoch's
//!   masternodes that created none of the blocks `number - 1 ..= number - (epoch - 1)`
//!   (`hooks.go:205-240`). The comeback part (`hooks.go:242-297`: masternodes penalised
//!   `LimitPenaltyEpoch + 1` epochs ago, back among the candidates, that signed none of the last
//!   `RangeReturnSigner` blocks) is appended to this list, and below `TIPRandomize` it is the
//!   whole answer (`hooks.go:297-301`). That part needs block bodies (sign transactions) and is
//!   XB-10; [`PenaltyEra::ComebackOnly`] returns nothing here.

use std::collections::BTreeMap;

use alloy_primitives::Address;

use crate::verify::HeaderError;

/// geth `MinimunMinerBlockPerEpoch` (`consensus/XDPoS/constants.go:60`).
pub const MINIMUM_MINER_BLOCK_PER_EPOCH: u64 = 1;
/// geth `MergeSignRange` (`consensus/XDPoS/constants.go:56`).
pub const MERGE_SIGN_RANGE: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenaltyEra {
    /// Before `TIPSigning`: sign counts come from the 0x89 contract.
    BlockSigners,
    /// From `TIPSigning` and `TIPRandomize`: sign counts are block creators.
    Creators,
    /// From `TIPSigning`, below `TIPRandomize`: only comebacks (XB-10) are penalised.
    ComebackOnly,
}

pub fn penalty_era(tip_signing: bool, tip_randomize: bool) -> PenaltyEra {
    match (tip_signing, tip_randomize) {
        (false, _) => PenaltyEra::BlockSigners,
        (true, false) => PenaltyEra::ComebackOnly,
        (true, true) => PenaltyEra::Creators,
    }
}

/// The blocks whose signers are counted for the checkpoint `number`, ascending. `tip2019` is the
/// `TIP2019` activation block.
pub fn scan_blocks(era: PenaltyEra, number: u64, epoch: u64, tip2019: u64) -> Vec<u64> {
    match era {
        PenaltyEra::BlockSigners => (number.saturating_sub(epoch)..number)
            .filter(|i| i % MERGE_SIGN_RANGE == 0 || *i < tip2019)
            .collect(),
        PenaltyEra::Creators => (number.saturating_sub(epoch.saturating_sub(1))..number).collect(),
        PenaltyEra::ComebackOnly => vec![],
    }
}

/// The penalty list of a checkpoint without comebacks, in the order of `prev_masternodes` (the
/// list of the previous checkpoint): every one with fewer than
/// [`MINIMUM_MINER_BLOCK_PER_EPOCH`] counted blocks. `sign_counts` counts, per address, the
/// blocks of [`scan_blocks`] it signed (0x89) or created. geth also lists a non-masternode miner
/// below the minimum (`hooks.go:231-235`); with a minimum of 1 no counted miner is below it.
///
/// No masternode list, or no counted block at all, is a typed error: scanning nothing would
/// penalise everyone.
pub fn penalties_v1(
    era: PenaltyEra,
    prev_masternodes: &[Address],
    sign_counts: &BTreeMap<Address, u64>,
) -> Result<Vec<Address>, HeaderError> {
    if era == PenaltyEra::ComebackOnly {
        return Ok(vec![]);
    }
    if prev_masternodes.is_empty() {
        return Err(HeaderError::MissingMasternodes);
    }
    if sign_counts.is_empty() {
        return Err(HeaderError::MissingSignData);
    }
    Ok(prev_masternodes
        .iter()
        .copied()
        .filter(|m| sign_counts.get(m).copied().unwrap_or(0) < MINIMUM_MINER_BLOCK_PER_EPOCH)
        .collect())
}
