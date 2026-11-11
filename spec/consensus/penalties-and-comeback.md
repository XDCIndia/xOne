# Penalties and the comeback of penalised masternodes

## Rule

**V2, at an epoch switch** (geth `HookPenalty`, run by `calcMasternodes`). Let `number` be the epoch-switch block, `parent` its parent. At `V2.SwitchBlock + 1` the list is empty. At or below `V2.SwitchBlock` it cannot be derived (error). Otherwise walk the parent chain from `parent` to the previous epoch-switch header, counting the blocks each coinbase mined; the switch block is listed (its hash is needed) but not counted.

1. A coinbase with fewer than `minimum` blocks is penalised, and so is a masternode of the epoch that mined none. `minimum` is 1 before `TIPUpgradePenalty` and the config's `minimumMinerBlockPerEpoch` from it.
2. **Before `TIPUpgradePenalty`**, when `number > (LimitPenaltyEpochV2 + 1) * epoch + V2.SwitchBlock` (`LimitPenaltyEpochV2` is 0): the nodes in the `Penalties` of the previous switch that are candidates now are *comeback candidates*. A comeback candidate that sent a sign transaction in the last 150 blocks (`RangeReturnSigner`, fewer if the epoch is shorter) is released; the others are added to the list (once).
3. **From `TIPUpgradePenalty`**, `limit` is the config's `limitPenaltyEpoch`, or 1 when that is 0 (`resolvePostUpgradeLimitPenaltyEpoch`). When `number > limit * epoch + V2.SwitchBlock`: count for each node in how many of the last `limit` switch headers it is listed. A node listed in the newest one is released only if it was listed in all `limit` of them **and** has at least `minimumSigningTx` qualifying sign transactions in the epoch (the last `epoch` blocks); otherwise it is penalised again, **also when it is penalised already for this epoch, so the list can hold an address twice**. With `ParoleDedup` each (signer, signed hash) pair counts once.

A sign transaction **qualifies** when it signs a block whose number is a multiple of `MergeSignRange` (15) that the scan has reached already, the block holding the transaction included. The scan runs in ascending block order; run backwards it finds fewer. The scan stops reading bodies as soon as nothing is left to decide (before the first block, and after each one).

The order of the list differs between geth runs (it follows a Go map); the header check compares as sets with duplicates (`CompareSignersLists`). Rust lists the nodes that mined too little in address order, then the silent masternodes in masternode order, then the others.

**Verification.** `Penalties` of an epoch-switch header must equal the derived list. If the list cannot be derived (a header or body is missing, the history is short) the answer is an error, never a pass (xOneGo#699 gap A). A non-empty `Penalties` at `V2.SwitchBlock + 1` is rejected (gap B).

**V1 from `TIPSigning`** (`HookPenaltyTIPSigning`, at a checkpoint `number >= epoch`). Over the `epoch - 1` blocks `number - 1 ..= number - epoch + 1` count the sealers; the masternodes of the header at `number - epoch` that sealed none are penalised, and a sealer below 1 block (none). Candidates that were in the `Penalties` of the header at `number - (LimitPenaltyEpoch + 1) * epoch` (`LimitPenaltyEpoch` 4; none while the chain is shorter) are comeback candidates, released as in V2 before the upgrade. From `TIPRandomize` the result is the penalised plus the remaining comeback candidates, before it the remaining comeback candidates only. Before `TIPSigning` the rule reads the 0x89 contract state and is not implemented.

## Source

- `eth/hooks/engine_v2_hooks.go:44-50` limit; `:318-565` the hook: `:346-366` the walk, `:366-385` the config, the minimum and the list, `:387-455` comeback before the upgrade, `:466-552` after it, `:500` `IsParoleDedup`, `:545` release test.
- `consensus/XDPoS/engines/engine_v2/engine.go:2019-2095` `calcMasternodes`, `:2054` the switch-plus-one case; `engine_v2/epochSwitch.go:26-93` the previous-epoch lookup and `ErrPenaltyAncestorMissing` (`consensus/XDPoS/utils/errors.go:110-119`); `engine_v2/verifyHeader.go:423` the check; `consensus/XDPoS/utils/utils.go:59` `CompareSignersLists`.
- `eth/hooks/hooks.go:182-302` V1; `:199-203` comeback window `(LimitPenaltyEpoch + 1) * epoch`; `:243-296` the scan.
- `common/constants.go:12-17` `LimitPenaltyEpoch`, `LimitPenaltyEpochV2`, `RangeReturnSigner`, `MinimunMinerBlockPerEpoch`; `common/types.go:76` `MergeSignRange`; `params/config.go:3422-3445` `IsTIPUpgradePenalty`, `IsParoleDedup`.
- `consensus/XDPoS/penalty.go` `CalculatePenalties` is not called by any hook and is not a rule.

## Networks

Apothem: `TIPUpgradePenalty` 83,600,000, `limitPenaltyEpoch` 5, `minimumMinerBlockPerEpoch` 5, `minimumSigningTx` 30 from round 27,360,000. Mainnet: unscheduled, the pre-upgrade rule. net2040: 16,200.

## Rust

`xone_consensus::penalty::{same_set, comeback::{scan, Release, Tally}, v2::{penalties, verify, post_upgrade_limit}, v1::{penalties, comeback_epoch}}`; readers `xone_consensus::chain::{BlockReader, PenaltyHistory}`.

## Deviations from geth

A missing body is `Error::Missing`; geth treats it as no signatures (`engine_v2_hooks.go:424-427`, `:513-518`). That can only turn a silent wrong list into a refused block.

## Not covered

The pre-`TIPSigning` V1 rule; the masternode selection that consumes the list (`calcMasternodes`, `removeItemFromArray`, XB-09); the retention of the lookback bodies (XA-09).
