//! The proportional payout from `TIPProportionalReward` (Apothem 84,733,000): a signer gets
//! `chainReward * min(sign, signSlots) / signSlots`, rounded down (geth
//! `CalculateRewardForSignerProportional`, `consensus/XDPoS/reward.go:289`). The part that
//! rounding or missing signatures leave unpaid is paid to no one. With no slots every signer is
//! kept with zero.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};

pub fn payout(
    chain_reward: U256,
    signers: &BTreeMap<Address, u64>,
    slots: u64,
) -> BTreeMap<Address, U256> {
    signers
        .iter()
        .map(|(&a, &sign)| {
            let paid = if slots == 0 {
                U256::ZERO
            } else {
                chain_reward * U256::from(sign.min(slots)) / U256::from(slots)
            };
            (a, paid)
        })
        .collect()
}
