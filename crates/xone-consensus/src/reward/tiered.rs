//! Reward 2.0 (`TIPFixedReward`, Apothem 83,600,000): masternodes, protectors and observers are
//! paid separately, each tier with the reward of the V2 config of the checkpoint's round
//! (`masternodeReward`, `protectorReward`, `observerReward`). Until `TIPProportionalReward` every
//! signer of a tier gets the whole tier reward; from it the proportional rule applies (geth
//! `HookReward`, `eth/hooks/engine_v2_hooks.go:93-212`, `CalculateRewardForSignerFixed`,
//! `consensus/XDPoS/reward.go:260`).
//!
//! The protector and observer sets are an input: the first `MaxProtectorNodes` of the standby pool
//! of the window's opening epoch, then the next `MaxObserverNodes` (`engine_v2_hooks.go:984-990`,
//! [`tiers`]). The MintedRecord bookkeeping (`:173-212`) and the burned-fee sum are not part of the
//! reward map and are not computed here.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use xone_chainspec::{ChainSpec, V2Config};

use super::{inflation, pay, proportional, tier_reward, Holders, RewardMap, RewardWindow, Split};
use crate::chain::Error;

/// The V2 config of the checkpoint's round.
pub fn config(spec: &ChainSpec, round: u64) -> Result<&V2Config, Error> {
    spec.xdpos
        .v2
        .as_ref()
        .ok_or(Error::NoV2)?
        .config_at_round(round)
        .ok_or(Error::NoV2Config(round))
}

/// Splits the standby pool (stake order, penalised nodes and masternodes already removed) into
/// the protector and observer sets of the config.
pub fn tiers(cfg: &V2Config, pool: &[Address]) -> (Vec<Address>, Vec<Address>) {
    let cap = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
    let p = cap(cfg.max_protector_nodes).min(pool.len());
    let o = p
        .saturating_add(cap(cfg.max_observer_nodes))
        .min(pool.len());
    (pool[..p].to_vec(), pool[p..o].to_vec())
}

/// `CalculateRewardForSignerFixed`: every signer gets the whole reward.
pub fn fixed(chain_reward: U256, signers: &BTreeMap<Address, u64>) -> BTreeMap<Address, U256> {
    signers.keys().map(|&a| (a, chain_reward)).collect()
}

/// The reward map of tiered checkpoint `number`; `cfg` is [`config`] at its round.
pub fn rewards(
    spec: &ChainSpec,
    number: u64,
    cfg: &V2Config,
    window: &RewardWindow,
    holders: &Holders,
    parent_root: B256,
) -> Result<RewardMap, Error> {
    let v2 = spec.xdpos.v2.as_ref().ok_or(Error::NoV2)?;
    let foundation = spec.xdpos.foundation_wallet;
    if foundation.is_zero() {
        return Err(Error::NoFoundation);
    }
    if number <= super::v1::checkpoint_period(spec) || number == v2.switch_block.saturating_add(1) {
        return Ok(RewardMap::new());
    }
    let proportional = spec.tip_proportional_reward.is_active(number);
    let t = window.tier_counts();
    let mut out = RewardMap::new();
    for (reward, signers) in [
        (cfg.masternode_reward, &t.masternode),
        (cfg.protector_reward, &t.protector),
        (cfg.observer_reward, &t.observer),
    ] {
        let chain_reward = inflation(spec, number, tier_reward(reward)?);
        let paid = if proportional {
            proportional::payout(chain_reward, signers, t.slots)
        } else {
            fixed(chain_reward, signers)
        };
        pay(
            holders,
            number,
            parent_root,
            foundation,
            &paid,
            Split::Upgrade,
            &mut out,
        )?;
    }
    Ok(out)
}
