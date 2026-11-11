# V1 checkpoint rewards, and the flat payout shared with V2 before the tiered rewards

## Rule

A checkpoint block is a multiple of `RewardCheckpoint` (900; the epoch when it is 0). The first reward is at the second checkpoint: a block `<= RewardCheckpoint` pays nothing, and a block below twice the period is an error in Rust (the unsigned subtraction wraps in geth).

**Window.** At checkpoint `n`, with period `r`, the rewarded blocks are `n - 2r + 1 ..= n - r`. The masternodes are the list in the `extraData` of the header at `n - 2r`. The sign transactions are read from every block `n - 2r + 1 ..= n - 1`, because a block is signed for in a later block. Blocks come from the parent chain of the checkpoint block, by hash, never from a store keyed by height (xOneGo `8be7d783`: the collector took side blocks and the root at 85,193,190 was wrong).

**Sign transaction.** To 0x89, exactly 68 bytes, selector `e341eaa4`, the signed hash in the last 32 bytes. Before `TIPSigning` (`effective.tip_signing`) it also needs a successful receipt; from it, presence is enough. The sender is the signer.

**Count.** For each rewarded block `i` that is a multiple of `MergeSignRange` (15) — or any block while TIP2019 is not active — the signers of block `i` are the senders of the sign transactions whose signed hash is the hash of `i`. Each sender that is in the masternode list counts once for that block (`+1` for the signer, `+1` for `totalSigner`).

**Payout.** `chainReward = Reward * 1e18`, halved by `RewardInflation` (at 2 years of blocks, again at 5, never from `TIPNoHalvingMNReward`). Each signer gets `chainReward / totalSigner * sign`: the division comes first, so `chainReward / totalSigner * totalSigner` is a little less than `chainReward`. With `totalSigner == 0` nobody is paid.

**Split.** Of a signer's reward, the owner (`validatorsState[signer].owner` in 0x88, **read from the parent state**: the state at the start of the checkpoint block, not after its transactions) gets `reward * 90 / 100`, the voters `reward * 0 / 100` shared by stake, the foundation wallet `reward * 10 / 100`. An unset owner is the zero address and is paid; there is no fallback to the signer. In this flat variant an owner that is the foundation wallet gets no 90% share (the 90% is burnt). The foundation amount is added to an existing entry and assigned otherwise. A voter with stake gets an entry of 0, and geth credits it: zero amounts are in the map (`AddBalance(x, 0)` touches the account).

**V2 before the tiered rewards.** The same payout and split apply from the V2 switch block (`number >= V2.SwitchBlock`). The window is found by walking the parent chain from `n - 1` for epoch-switch headers (V2: `IsEpochSwitch`; a V1 header: a multiple of the epoch): the first one found closes the rewarded epoch (`end` is the block before it), the second opens it (`start` is the block after, its header lists the masternodes). The block `V2.SwitchBlock + 1` is never counted as a switch. At exactly `V2.SwitchBlock` the V1 window is used. At `V2.SwitchBlock + 1` the reward is empty. Sign transactions are counted by presence only.

## Source

- `consensus/XDPoS/reward.go:51` `isSigningTx`; `:71` `GetRewardForCheckpoint`; `:80-82` window; `:131-144` the receipt rule before `TIPSigning`; `:190-221` the count and its TIP2019 filter; `:228` `CalculateRewardForSigner`; `:473` `GetRewardBalancesRate`, `:499` owner-is-foundation skip, `:505-541` voters, `:545-549` foundation accumulate; `:640-644` zero amounts are credited.
- `eth/hooks/engine_v2_hooks.go:60-70` hook entry (empty foundation is an error), `:76` the switch-plus-one skip, `:219-290` the flat path, `:245-248` `isV2`, `:621` `GetSigningTxCount`, `:642-675` the V1 window, `:680-720` the epoch-switch walk, `:826-860` the count.
- `eth/util/util.go:18` `RewardInflation`; `common/constants.go:20` `OneYear`; `consensus/XDPoS/constants.go:26-28` the percentages; `common/types.go:76` `MergeSignRange`.
- `core/state/statedb_utils.go:114-157` the 0x88 owner, voters and stake; Rust `xone_state::system_contracts::validator_0x88`.

## Networks

Apothem (51) and mainnet (50) from block 1,800 until `TIPFixedReward` (Apothem 83,600,000; mainnet unscheduled, so mainnet V2 stays on this payout). Devnets by their rows.

## Rust

`xone_consensus::reward::{rule, RewardWindow, Holders, inflation, v1::{locate, payout, rewards}, v2::{locate, rewards}}`, inputs from `xone_consensus::chain` (`SignTxCounter`, `BlockReader`). `Holders` carries the state root it was read at and the engine refuses any other than the parent's (`Error::WrongState`). An absent header or body is `Error::Missing`, never a count of zero (geth logs a missing body and counts none: `engine_v2_hooks.go:749-752`, `:1003`; V1 `reward.go:153-157`). Tests `crates/xone-consensus/tests/reward_penalty.rs`, vectors `conformance/vectors/reward/`.

## Not covered

MintedRecord bookkeeping (`engine_v2_hooks.go:173-212`) and the burned-fee sum; the voters of real checkpoints (the archive vectors carry owners only; voters are covered by synthetic cases against the oracle); the pre-`TIPSigning` receipt rule has no archive vector.

## Legacy

`ApplyRewards` (`reward.go:554`) is not wired by any node; the hook above is.
