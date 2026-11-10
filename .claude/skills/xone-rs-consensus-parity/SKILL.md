---
name: xone-rs-consensus-parity
description: The XDPoS V1 and V2 rules checklist for the xOne Rust rebuild, each rule mapped to its geth-XDC file and line. Covers rewards, penalties and comeback, masternode selection, leader selection, votes, timeouts, QC and TC, SyncInfo, sign transactions and side-branch handling. Use when writing a spec page, reviewing a consensus PR, planning M2, M3 or M6, checking a producer before a mint test, or when the task says XDPoS, consensus parity, epoch switch, penalty, reward, QC, TC, timeout, vote, sign tx, 0x89, masternode order.
---

# xone-rs-consensus-parity

A checklist, not a description. Each row is a rule that at least one XDC client got wrong on a live network. A row is closed only by vectors and a differential result.

## Operating rules

1. Line numbers are a snapshot at geth-XDC `main` `5d6ba3664`. Re-anchor them at the commit in `conformance/PIN` when you write the spec page.
2. Read the geth function, then write the spec page, then the vector, then the code. Do not port from xOneGo or reth-xdc code; use them only as a list of mistakes.
3. A follower may not use an approximation that a producer could not use. `Validators` and `Penalties` are bit-exact for both.
4. Missing input is a typed error (geth: `ErrPenaltyAncestorMissing`, `consensus/XDPoS/utils/errors.go:119`), never an empty list.
5. `Round`, `Epoch`, `BlockNumber` and `GapNumber` are separate types. The V2 config is looked up by `Round`.

## Checklist

### Header and epoch

| # | Rule | geth-XDC |
|---|---|---|
| H1 | V2 header verification, incl. `Validators` and `Penalties` at an epoch switch | `consensus/XDPoS/engines/engine_v2/verifyHeader.go:74` |
| H2 | Epoch switch detection by round | `engine_v2/engine.go:1952` (`IsEpochSwitch`) |
| H3 | Epoch switch info and its cache | `engine_v2/engine.go:2451` (`getEpochSwitchInfo`) |
| H4 | Masternodes for the epoch come from the switch header, verbatim | `engine_v2/engine.go:1656` |
| H5 | Local masternode computation and penalties | `engine_v2/engine.go:2023` (`calcMasternodes`) |
| H6 | Gap-pool order: bootstrap gaps use the V1 checkpoint order, later gaps the stake-sorted order | xOneGo PR 696 explains the defect; verify in `engine_v2` and `engine_v1/snapshot.go` |
| H7 | Leader: `round % epoch % len(masternodes)` | `engine_v2/engine.go:869`, `:900` (`YourTurn`, `yourturn`) |
| H8 | Header lookup by hash covers side branches | `core/blockchain_reader.go:84`, `core/headerchain.go:493` |

### Rewards

| # | Rule | geth-XDC |
|---|---|---|
| R1 | Reward hook wiring, V1 and V2 | `eth/hooks/hooks.go:41`, `eth/hooks/engine_v2_hooks.go:58` |
| R2 | Sign-tx count over the reward window, from block bodies | `eth/hooks/engine_v2_hooks.go:621` (`GetSigningTxCount`) |
| R3 | Tiered count after the reward upgrade | `eth/hooks/engine_v2_hooks.go:920` |
| R4 | Per-signer reward: fixed, then proportional from `TIPProportionalRewardBlock` | `consensus/XDPoS/reward.go:228`, `:260`, `:289` |
| R5 | Split between owner and voters | `consensus/XDPoS/reward.go:393` |
| R6 | Only canonical blocks feed the reward window | xOneGo PR 738 (the defect) |
| R7 | The reward window after a snap start needs bodies below the pivot | `eth/downloader/downloader.go:658`, `:714` |
| R8 | MintedRecord and similar keys use the real `SwitchEpoch` | reth-xdc defect; verify in `reward.go` |

### Penalties and comeback

| # | Rule | geth-XDC |
|---|---|---|
| P1 | Penalty hook, scan order oldest-first | `eth/hooks/engine_v2_hooks.go` (hook body), `engine_v2/epochSwitch.go:26-93` |
| P2 | `RangeReturnSigner` = 150, `LimitPenaltyEpoch` | `consensus/XDPoS/constants.go:44`, `:57` |
| P3 | After the penalty upgrade: release needs the last N switch headers and a minimum of sign txs | `engine_v2/epochSwitch.go:93`, `consensus/XDPoS/penalty.go:32` |
| P4 | Missing ancestor is an error, not an empty list | `engine_v2/epochSwitch.go:80`, `engine_v2/verifyHeader.go:253` |
| P5 | The V2 config (timeouts, thresholds) is selected by round | `params/config.go` V2 `AllConfigs` |

### Votes, timeouts, certificates

| # | Rule | geth-XDC |
|---|---|---|
| V1 | Vote verification | `engine_v2/vote.go:23` |
| V2 | Voting rule: round greater than highest voted round, lock-QC rule | `engine_v2/vote.go:404` (`verifyVotingRule`) |
| V3 | Send vote; highest voted round is persisted before the vote leaves | `engine_v2/vote.go:217` |
| V4 | QC verification and processing; commit rule | `engine_v2/utils.go:315`, `:476`, `:530` |
| V5 | Timeout verification, handling, sending | `engine_v2/timeout.go:22`, `:99`, `:225` |
| V6 | Round timer | `engine_v2/timeout.go:300` (`OnCountdownTimeout`) |
| V7 | TC verification and processing | `engine_v2/timeout.go:373`, `:474` |
| V8 | SyncInfo verification; a blank bootstrap TC is accepted | `engine_v2/utils.go:672`, `:739` (`isBlankTC`) |
| V9 | Proposed block handling; a sibling at the same height does not get a second vote | `engine_v2/utils.go:758` |

### Producer

| # | Rule | geth-XDC |
|---|---|---|
| B1 | Prepare and Finalize of a V2 block | `engine_v2/engine.go:651`, `:769` |
| B2 | `0x89` sign tx creation and emission, also while penalized | `contracts/utils.go:123` |
| B3 | Special transactions are gasless and are included first | `core/types/transaction.go:745` |
| B4 | Base fee of a produced block uses the XDC formula | spec page from `consensus/misc` at the pin |

## Steps

1. Pick the rows of the milestone (M2: V1 parts of H, R, P; M3: H, R, P, V1-V9 as a follower; M6: B rows and the sending side of V).
2. For each row: re-anchor, write `spec/<row>.md`, export vectors, add the mutant.
3. Run the function-level differential through the sidecar oracle on every Apothem epoch switch in range.
4. Tick the row on the milestone issue with range and commit.

## Done

Every row in scope has a spec page, vectors, a detected mutant and a 0-mismatch result over all epoch switches in the milestone range.
