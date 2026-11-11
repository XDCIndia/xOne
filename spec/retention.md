# Retention: how far below a block every rule reads

## Rule

A node that starts from a snapshot at a pivot block `P` has the state of `P` and nothing below it except what it was given. Every rule that runs on a block above `P` and reads below it needs that data to be there. The data is a function of the rule and of the chain spec, not a constant ("2 epochs", "10 GB"). This page lists every rule that looks back, what it reads, and the longest lookback.

**Terms.** `n` is the block a rule runs on; the lookback is `n - (lowest block it reads)`. `E` = `XDPoS.epoch`, `G` = `XDPoS.gap`, `r` = `rewardCheckpoint` (0 means `E`), `S` = V2 switch block, `W` = `RangeReturnSigner` = 150, `LPE` = `LimitPenaltyEpoch` = 4 (V1), `c` = `MergeSignRange` = 15. Apothem: `E` 900, `G` 450, `r` 900, `S` 56,828,700. Mainnet: the same `E`, `G`, `r`; `S` 80,370,000.

**An epoch holds at most `E` blocks.** V2 epochs are counted in rounds (`IsEpochSwitch`, `engine.go:1952`) and a round makes at most one block, so the walk to the `k`-th epoch switch below `n` is at most `k * E` blocks, and a rule that fires at an epoch switch fires within `E` blocks of any block. On the Apothem archive the spacing of 2,532 consecutive switches (83,000,425 to 85,245,161) is 12 to 900 blocks; the largest two-switch span is 1,800 and the largest five-switch span is 4,500, so both bounds are reached (`tests/bootstrap/apothem-epoch-switches-83000425-85245161.json`, test `real_epoch_spacing_stays_inside_the_derived_bounds`).

## The table

One row per rule. "Reads" lists headers (H), bodies (B), receipts (R) and gap snapshots (S, the candidate pool stored at a gap block, derived from the 0x88 state at that block). Lookbacks are in blocks below `n`; the value is the Apothem and mainnet number where `E` = 900, `G` = 450. Source lines are in the geth-XDC tree (see "Sources" at the end).

| # | Rule, on block `n` | Reads | Max lookback | Value | Source |
| --- | --- | --- | --- | --- | --- |
| 1 | V1 reward, checkpoint `n <= S` | H and B of `n-2r+1 ..= n-1`, H of `n-2r` (the masternodes); R of the same blocks while one is below `TIPSigning` (effective 3,000,000) | H `2r`, B `2r-1`, R `2r-1` | 1800, 1799, 1799 (2 epochs) | `consensus/XDPoS/reward.go:80` window, `:91` loop, `:131-145` receipts before `TIPSigning`, `:183` masternodes; V1 branch `engine_v2_hooks.go:651-675` |
| 2 | V2 flat reward, switch `n`, before `TIPFixedReward` (mainnet today) | H `s2 ..= n-1` to find the second switch `s2` below `n`; B `s2+1 ..= n-1`; masternodes from H `s2` (`Validators`, or the V1 `extraData` when `s2` is a V1 checkpoint); no receipts | H `2E`, B `2E-1` | 1800, 1799 | `engine_v2_hooks.go:621` (`GetSigningTxCount`), `:681` (`rewardEpochCount, signEpochCount := 2, 1`), `:689-720` walk, `:700` the V2 switch block plus one is not counted, `:711` masternodes, `:728-770` bodies, `:609-619` stored block, presence only |
| 3 | V2 tiered reward, `TIPFixedReward` or `TIPProportionalReward` (Apothem 83,600,000 and 84,733,000) | as 2, plus the standby pool of the epoch of `s2` for protectors and observers: S at gap block `floor(s2/E)*E - G`, and its H | H and S `3E+G-1`, B `2E-1` (the pool only while some config has `maxProtectorNodes + maxObserverNodes > 0`) | 3149, 1799 | `engine_v2_hooks.go:93-212` gate, `:920` `GetSigningTxCountTiered`, `:950-996` walk, `:985` `GetStandbynodes(h)`, `:999-1012` bodies; `engine.go:3351-3375` `GetStandbynodes`, `:915-927` gap number. The proportional row changes the sum (`reward.go:289`), not the data |
| 4 | V1 penalty, checkpoint `n <= S`, from `TIPSigning` | H `n-E+1 ..= n-1` (sealers), H `n-E` (previous masternodes), H `n-(LPE+1)*E` (`Penalties` for the comeback), B of the last `W` blocks, the signer snapshot of gap block `n-G` | H `(LPE+1)*E`, B `W`, S `G` | 4500, 150, 450 | `eth/hooks/hooks.go:182-302`, `:200` `comebackLength`, `:260` the `RangeReturnSigner` scan; `common/constants.go:12` `LimitPenaltyEpoch`, `:16` `RangeReturnSigner` |
| 5 | V2 penalty before `TIPUpgradePenalty`, switch `n > S` | H back to the previous switch (counts coinbases); the previous switch's `Penalties` (`back` 0, `LimitPenaltyEpochV2` 0); B of the last `W` blocks, the comeback, once `n > E + S` | H `E`, B `min(W, E)` | 900, 150 | `engine_v2_hooks.go:346-366` walk, `:367` masternodes of the parent, `:390-433` comeback and `:409` `startRange := common.RangeReturnSigner - 1`; `epochSwitch.go:28-52` |
| 6 | V2 penalty from `TIPUpgradePenalty` (Apothem 83,600,000), switch `n` | `Penalties` of the last `limit` switches (`back` 0 ..= `limit-1`, one epoch of header walk per step), B of the epoch that ends, once `n > limit*E + S`; `limit` = `limitPenaltyEpoch` of the config at the parent's round, 1 when 0 | H `limit * E`, B `E` | `limit` 5: 4500, 900 | `engine_v2_hooks.go:471` `limit`, `:479` the loop, `:501` `startRange := Epoch - 1`, `:502-543`; `epochSwitch.go:34` one hop per step, `engine.go:2624-2630` header-by-header recursion to the switch; the walk fails with `ErrPenaltyAncestorMissing` when a header is absent (`epochSwitch.go:65-83`, `utils/errors.go:110-119`) |
| 7 | V2 gap snapshot read at a switch `n` (`calcMasternodes`) | S at gap block `n - n%E - G` and H of it; in the first V2 epoch the pool is the V1 list in the `extraData` of `S` | H and S `E-1+G` | 1349 | `engine.go:915-927` `getSnapshot`, `:2023-2036` `calcMasternodes`, `:1174-1420` the bootstrap from the switch header; after a snap pivot a pool below the pivot is `ErrV2SnapshotMissing` (`:1121-1133`, `:1148-1159`, #1931) |
| 8 | V1 signer snapshot (`recents`) at block `n` | H from the last gap block `g` (`(g+G) % E == 0`) to `n-1`, S at `g`; `recents` keeps `len(signers)/2 + 1` creators | H and S `E` | 900 | `xdpos.go:2305-2345`, `:78` `snapshotMaxWalkBack` 2,000,000 (geth's own cap, not a rule); `snapshot.go:185` |
| 9 | 3-chain commit (`commitBlocks`) of the block a QC names | H of its parent and grandparent | H 3 | 3 | `engine_v2/utils.go:530-598`, called from `processQC` `:513`; the highest commit is memory only (`engine.go:101`) |
| 10 | V2 QC verification | H of the epoch switch of the block the QC names, for its masternodes and the gap number | H `E` | 900 | `engine_v2/utils.go:315-322`, `:373` threshold, gap check after it; `verifyHeader.go:169` |
| 11 | TC verification (epoch walk-back) | H of the switch of the TC's epoch (`getTCEpochInfo` walks back while `epochRound > tc.round`), S at `tc.gap_number` (must be non-empty) | H `2E`, S `3E+G-1` for a TC of the previous epoch; geth has no bound | 1800, 3149 | `engine_v2/timeout.go:334-362`, `:380`; a sync info is verified only when newer than the local one (`utils.go:672-733`) |
| 12 | Reorg depth | unwind and re-execute above the committed floor; state and changesets of every block above it | none below the pivot: the floor is the anchor | head - `P` | geth has no committed-floor check in `core/blockchain.go:3433` (`reorg`); `highestCommitBlock` `engine_v2/utils.go:584`; xOne: ADR-005 section 10, `BadUnwindTarget` below the first retained block |
| 13 | `XDPoS_getMasternodesByNumber(b)` | masternodes: H of the switch of `b` (`E-1`); penalties: the switch H; standby pool: S at the gap block of that switch and its H | H and S `2E+G-2` | 2248 | `consensus/XDPoS/api_extended.go:171-215` |
| 14 | `XDPoS_getMissedRoundsInEpochByBlockNum(b)` | H of the switch of `b` | H `E` | 900 | `api_extended.go:328-348`, `engine_v2/utils.go:824-834` |
| 15 | `XDPoS_getEpochNumbersBetween(a, b)` | the switch infos of the whole range, up to 1,000 epochs | the range, not a lookback | up to 900,000 | `api_extended.go:285-322`, `:296` |
| 16 | `eth_getLogs(from, to)` | R of every block in the range | the range; none below `P + 1` on a V2 pivot | `P + 1` | `eth/filters/filter.go:383` `rangeLogs`, `:429`, `:459` `blockLogs` |

Not rules, but read by every node and counted here: the state of `P` (the account and storage trie, the 0x88 and 0x89 contracts and the minted record that the tiered reward reads, `engine_v2_hooks.go:179-205`) is the pivot state itself. Nothing below it is kept.

## What geth does about it

geth-XDC does not derive the window; it hard-codes two epochs and relaxes the checks inside it.

- `A.68.2` pre-fetches `2 * Epoch` = 1800 ancestor headers below the prime window (`eth/sync_xdc.go:1336-1349`, `:4326-4334`). `A.72` primes bodies and receipts for `[pivot - 2*Epoch, pivot]`, a later fix widened the scan to `3 * Epoch` (`sync_xdc.go:3412-3424`, `eth/downloader/downloader.go:700-714` `PrimeAncestorBodies`). Row 6 needs `limit * E`, so a 1,800 header window is short on Apothem and the penalty walk fails with `ErrPenaltyAncestorMissing`, which the comment at `epochSwitch.go:18-27` names as "depth less than the penalty lookback window".
- `InsertHeadersBeforeCutoff` is in `core/blockchain.go:3872`, not in `core/blockchain_xdc.go`. It writes the headers to the freezer and calls `TruncateTail` for bodies and receipts at `last + 1` (`:3927`): nothing below the pivot but headers.
- `inCheckpointCatchup` (`engine_v2/utils.go:281-311`, window `Epoch * 2` at `:311`) turns off `verifyQC` (`verifyHeader.go:169`) and the 3-chain commit (`utils.go:537`) for every block `<= anchor + 2*Epoch`. The two-epoch window of data is also a two-epoch window without certificate checks. xOne keeps the window and does not relax a check.
- A snapshot below the pivot is refused after a snap sync, because it holds only the masternodes of the epoch switch header and not the contract pool (`engine.go:1121-1133`, issue #1931). Rows 3, 7 and 11 therefore need the gap snapshots in the manifest, not only headers.

## The derived window

For a pivot `P` the window of each class is the largest need over every rule that can run on the first `E` blocks after `P`. A rule on block `n = P + 1 + j` that reads `l` blocks below it needs `l - j` blocks at and below `P`:

```
window(class, P) = max over j in 0 .. E-1, over every rule that runs on P+1+j, of (lookback(rule, class, P+1+j) - j)
floor(class, P)  = P + 1 - window(class, P)        first block of the class that is kept
```

`j = 0` is in the range because a round can skip to the next epoch at any block. Fork rows inside the range are evaluated on both sides, so a pivot `E` blocks before `TIPUpgradePenalty` already keeps the new window (test `the_window_grows_at_the_upgrade_and_not_at_the_proportional_row`). `limit` and the tier counts are the maximum over the V2 configs that a block can run under: its round is at least `n - S` (every block has a higher round than its parent) and has no upper bound, so every config from the one at round `n - S` on counts. Code: `xone_sync::bootstrap::{Rule::lookback, Retention::derive}`.

**Apothem, pivot 84,000,000** (after `TIPUpgradePenalty`, `limitPenaltyEpoch` 5, tiers from round 27,360,000):

| class | window | set by | arithmetic |
| --- | --- | --- | --- |
| headers | 4500 | row 6 | `limit * E` = 5 * 900 |
| bodies | 1799 | row 3 | `2E - 1` = 2 * 900 - 1 |
| receipts | 0 | none | `TIPSigning` is 3,000,000 |
| gap snapshots | 3149 | row 3 | `3E + G - 1` = 2700 + 450 - 1 |
| state | the pivot | | |

Plan at the pivots the issue names (`xone_sync::bootstrap::plan_at`, tests `plan_at_*`): at `P` = 83,600,000 the headers are 83,595,501 ..= 83,600,000, the bodies 83,598,202 ..= 83,600,000 and the gap snapshots 83,596,950, 83,597,850, 83,598,750, 83,599,650; at 84,733,000 the same lengths (the proportional row adds no data); at the gap block 85,000,050 the gap snapshot of the pivot itself is in the plan.

**Apothem before `TIPUpgradePenalty`, and mainnet today** (no `TIPUpgradePenalty`, no `TIPFixedReward`, `limitPenaltyEpoch` 0): headers 1800 (row 2), bodies 1799 (row 2), receipts 0, gap snapshots 3149 (row 11). Mainnet at 100,000,000: headers 99,998,201 ..= 100,000,000, gap snapshots 99,997,650, 99,998,550, 99,999,450. When mainnet schedules `TIPUpgradePenalty` with `limitPenaltyEpoch` 5 the header window is 4500 with no code change (test `the_window_is_derived_from_the_spec_not_a_constant`).

**V1 pivots.** Below `TIPSigning` the reward window keeps receipts (`2r - 1` = 1799, row 1); from it the V1 penalty reads 4500 headers (row 4). At the V2 switch the first V2 reward window opens on the V1 checkpoint `S - E` (`engine_v2_hooks.go:681-711`: the walk from the first V2 reward block finds `S`, then `S - E`), so a pivot above `S` still needs V1 headers and bodies; the ranges include them.

## What "two epochs" and "10 GB" cover

- **Two epochs (1800 blocks)** is row 2: the headers of the V2 flat reward. It covers mainnet today, and the bodies of every network (1799). It does not cover the Apothem penalty walk (4500 headers), the gap snapshots of any V2 network (3149), or mainnet after it schedules the upgrade. It is also not a statement about which blocks: 1800 below the head moves while the pivot is fixed.
- **10 GB** is a size, not a lookback. Sampled sizes on the archive (2026-11-11): a block with no transactions is 4,942 bytes (the QC in `extraData` is 4,352), one with 94 sign transactions is 20,944 bytes, about 6 KB a block on average. 4500 blocks is about 27 MB, so 10 GB is 370 times the Apothem window in bytes and says nothing about which block is the first one kept. It covers the data of rows 1 to 11 by a wide margin and none of the gap snapshots, which are not blocks, and it gives no rule for what to refuse. A byte budget that prunes the oldest first can remove a block inside the window; the floors above are in blocks.

## Typed refusal

A request or a rule that needs data below its floor gets `Missing::UnsupportedHistory { class, block, floor, pivot }` (`xone_sync::bootstrap::Retention::is_within`, `serves`), never an empty list, a zero count or a default. The consensus engines already do the same for their own inputs: `chain::Error::Missing` for an absent header or body, `v2::Error::Missing` for an absent pool or QC parent. Tests exercise every row at its floor and one block below it, and run the reward, penalty, gap snapshot and 3-chain engines on readers that hold exactly the floor and one block less.

## Growing and shrinking

A new rule or a longer `limitPenaltyEpoch` in the chain spec grows a window. A snapshot records the first block it holds per class (the `first_block` of the data manifest, ADR-005 section 6.3); an importer compares it with `Retention::floor` and refuses a snapshot that holds less. Growth needs no migration. Keeping less than the manifest of an older binary assumed is a shrink: it needs a new manifest format version, so that an older binary refuses at open and not at the first reward block (ADR-005 section 12).

## Not covered

The pivot-specific tightening from the anchor's own epoch layout (the plan assumes the first block after the pivot can open an epoch and the previous switch is `E` below the current one; with the switch numbers of the anchor the headers of row 6 shrink by up to `E - 1` blocks); the importer that checks `first_block` against the floors (XB-18); the first post-pivot replay (XA-05); the TC rule when a sync info carries a TC older than two epochs (row 11: refused here, and whether the QC alone is then processed is the consensus owner's call).

## Sources

geth-XDC at `conformance/PIN` (`6fc8e8c2088b7f18264f5663036669d04fa7becf`). The lines above are read from the tree in the session scratchpad, which has no `.git`; the line `engine_v2_hooks.go:621` that the issue cites at the pin matches it. Re-read every line with `conformance/fetch-geth.sh` before moving the pin. Archive: `xdc02:21105`, geth-XDC `v1.17.10-xdc.20`, read-only, head 85,245,927 on 2026-11-11.
