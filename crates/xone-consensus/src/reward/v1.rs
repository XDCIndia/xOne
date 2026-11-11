//! The V1 checkpoint reward and the flat payout that V2 shares until the tiered rewards start.
//!
//! At checkpoint `n` (a multiple of `RewardCheckpoint`) the blocks `n - 2r + 1 ..= n - r` are
//! rewarded, and the masternodes are those listed in the header of `n - 2r` (geth
//! `GetRewardForCheckpoint`, `consensus/XDPoS/reward.go:71`). The chain reward is
//! `Reward * 1e18`, halved by [`super::inflation`]; each signer gets
//! `chainReward / totalSigner * sign` (`reward.go:228`), then 90% to its owner and 10% to the
//! foundation (`reward.go:473`, `:499`).

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use xone_chainspec::ChainSpec;

use super::{inflation, pay, Counts, Holders, RewardMap, RewardWindow, Split, ETHER};
use crate::chain::Error;

/// The reward period: `RewardCheckpoint`, or the epoch when it is zero (`engine_v2_hooks.go:62-65`).
pub fn checkpoint_period(spec: &ChainSpec) -> u64 {
    match spec.xdpos.reward_checkpoint {
        0 => spec.xdpos.epoch,
        r => r,
    }
}

/// The rewarded blocks of checkpoint `number` and the checkpoint whose header lists the masternodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    pub start: u64,
    pub end: u64,
    pub checkpoint: u64,
}

/// `reward.go:80-82`. The first reward is at the second checkpoint; an earlier block is an error
/// here (geth would wrap the unsigned subtraction).
pub fn locate(spec: &ChainSpec, number: u64) -> Result<Located, Error> {
    let r = checkpoint_period(spec);
    let checkpoint = number.checked_sub(2 * r).ok_or(Error::ShortChain(number))?;
    let start = checkpoint + 1;
    Ok(Located {
        start,
        end: start + r - 1,
        checkpoint,
    })
}

/// `CalculateRewardForSigner` (`reward.go:228`): the integer division comes first.
pub fn payout(chain_reward: U256, counts: &Counts) -> BTreeMap<Address, U256> {
    if counts.total == 0 {
        return BTreeMap::new();
    }
    let per = chain_reward / U256::from(counts.total);
    counts
        .signs
        .iter()
        .map(|(&a, &n)| (a, per * U256::from(n)))
        .collect()
}

pub(super) fn flat(
    spec: &ChainSpec,
    number: u64,
    window: &RewardWindow,
    holders: &Holders,
    parent_root: B256,
    receipts: bool,
) -> Result<RewardMap, Error> {
    let foundation = spec.xdpos.foundation_wallet;
    if foundation.is_zero() {
        return Err(Error::NoFoundation);
    }
    if number <= checkpoint_period(spec) {
        return Ok(RewardMap::new());
    }
    let chain_reward = inflation(
        spec,
        number,
        U256::from(spec.xdpos.reward) * U256::from(ETHER),
    );
    let counts = window.flat_counts(spec, receipts);
    let mut out = RewardMap::new();
    pay(
        holders,
        number,
        parent_root,
        foundation,
        &payout(chain_reward, &counts),
        Split::Flat,
        &mut out,
    )?;
    Ok(out)
}

/// The reward map of V1 checkpoint `number`. `window` must be the one [`locate`] describes and
/// `holders` the snapshot of the parent state, whose root is `parent_root`.
pub fn rewards(
    spec: &ChainSpec,
    number: u64,
    window: &RewardWindow,
    holders: &Holders,
    parent_root: B256,
) -> Result<RewardMap, Error> {
    let at = locate(spec, number)?;
    if (window.start(), window.end()) != (at.start, at.end) {
        return Err(Error::NotContiguous {
            start: at.start,
            number: window.start(),
        });
    }
    flat(spec, number, window, holders, parent_root, true)
}
