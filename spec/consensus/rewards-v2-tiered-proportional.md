# Reward 2.0: the three tiers, the fixed payout, and the proportional payout

## Rule

From `TIPFixedReward` or `TIPProportionalReward` (`IsTIPTieredReward` is the OR of the two rows; Apothem 83,600,000 and 84,733,000) the checkpoint pays three tiers separately: masternodes, protectors and observers. The rule is chosen by the fork rows at the number of the checkpoint block. Gas2500x (Apothem 83,600,000) sets the base fee; it changes the fee credit and the burnt amount of the block but not one amount in the reward map.

**Window and sets.** The window is found as for V2 before the tiers (`rewards-v1.md`): the second epoch-switch header opens it. The masternodes are that header's list. The protectors are the first `MaxProtectorNodes` of the standby pool of that header, the observers the next `MaxObserverNodes`; the pool is the candidates of the epoch's snapshot in stake order without that epoch's masternodes and penalties (`GetStandbynodes`). The pool is an input.

**Count.** Only blocks that are a multiple of 15 count; there is no TIP2019 case. `signSlots` is the number of those blocks in the window, whether signed or not. Each tier counts its own members: a block adds 1 to a member of a tier that signed it (once per block).

**Tier reward.** `chainReward = floor(reward * 1e18)` computed as geth does it: `big.Float.Mul(SetFloat64(reward), SetUint64(1e18))`, which rounds the product to **64 mantissa bits, nearest even**, then truncates. 63.42 is paid as 63420000000000001704, not as 63.42e18 (the float is 63.4200000000000017053…). `reward` is the config's `masternodeReward`, `protectorReward` or `observerReward` at the round of the checkpoint block (`config_at_round`). Then `RewardInflation` as before.

**Fixed (from `TIPFixedReward`).** Every signer of a tier gets the whole tier reward, whatever it signed.

**Proportional (from `TIPProportionalReward`).** A signer gets `chainReward * min(sign, signSlots) / signSlots`, rounded down. With `signSlots == 0` every signer is kept with 0. The unpaid part is paid to no one. A node that signed 59 of 60 slots gets 59/60 of the tier reward; before 84,733,000 it got all of it. Besu and nethermind missed this fork and paid 23 of 73 owners the wrong amount.

**Split.** As in `rewards-v1.md` with two differences: the owner always gets the 90% share, also when it is the foundation wallet, and the foundation share is added to an existing entry.

## Source

- `eth/hooks/engine_v2_hooks.go:93-212` the tiered branch of `HookReward`, `:133-138` the float, `:140` `RewardInflation`, `:143-146` fixed or proportional, `:920` `GetSigningTxCountTiered`, `:949-995` the walk and the sets, `:984-990` the tier split, `:1022-1043` the count and `signSlots`; `:1075` `legacyStandbyPool` (devnet 5551 only, not implemented).
- `consensus/XDPoS/reward.go:260` `CalculateRewardForSignerFixed`; `:289` `CalculateRewardForSignerProportional`; `:333` `GetRewardBalancesRateUpgrade`, `:341` the owner is always paid, `:378` the foundation gate.
- `params/config.go:3385-3413` `IsTIPFixedReward`, `IsTIPProportionalReward`, `IsTIPTieredReward`; `:1048-1051` Apothem rows; `:2465-2490` `V2Config`.
- `consensus/XDPoS/network_constants.go:86-97`, `common/types.go:111-113` the Apothem constants; mainnet `TIPUpgradeReward` is the placeholder 9,999,999,999.

## Networks

Apothem from 83,600,000 (fixed) and 84,733,000 (proportional). net2040 from its own `tipFixedRewardBlock` 16,200 (no proportional row). Mainnet: unscheduled.

## Rust

`xone_consensus::reward::{rule, tier_reward, tiered::{config, tiers, fixed, rewards}, proportional::payout}`.

## Not covered

The MintedRecord and burnt-fee bookkeeping (`:173-212`); `legacyStandbyPool` of devnet 5551; protector and observer tiers on the archive, because the Apothem checkpoints sampled have an empty pool (no standby beyond the masternodes and penalties): those two tiers are checked by synthetic cases against the oracle only.
