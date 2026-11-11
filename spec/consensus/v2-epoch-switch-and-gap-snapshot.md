# V2 epoch switch, gap snapshot and the lists of a switch header

## Rule

**Detection is by round, not by number** (`IsEpochSwitch`). The V2 switch block is a switch (epoch number `number / epoch`). A V1 block is not. The first V2 block, whose QC certifies the V2 switch block, is a switch. Any other V2 block is a switch when the round of its QC is below the start round of its own epoch, `round - round % epoch`. The epoch number is `switchEpoch + round / epoch`. Timeouts make an epoch longer than `epoch` blocks, so a switch can be at any block number, and a block number that is a multiple of `epoch` is usually not one. For a block that does not exist yet the producer asks `isEpochSwitchAtRound`: true when the parent is the V2 switch block; false when `round <= parentRound`; else `parentRound < round - round % epoch`.

**The gap block** of the epoch that holds `number` is `number - number % epoch - gap`, or 0 when the epoch start is not above `gap`. For a switch whose number is itself `epoch - gap` modulo `epoch`, `calcMasternodes` reads the parent's gap (`snapNum--`); the base is the same.

**The gap snapshot** (`SnapshotV2`). A node that executes the gap block reads the candidates of 0x88 (array at slot 8, zero words skipped) and the cap of each (`validatorsState[c].cap`) from the state of that block, drops the zero address, and sorts by cap, largest first, with the frozen quicksort (`xdc_sort.Slice`, comparator `cap_i >= cap_j`). Equal caps stay in the order that sort leaves them, so a stable or a standard sort is wrong at pool width (the 464-candidate vector). The pool is kept whole: no cap and no penalty is applied yet. No candidate at all is `Missing` (geth looks for another source; here the driver decides, and nothing is guessed).

**The first V2 epoch has no V2 gap block.** When `number - number % epoch` is the V2 switch block (every switch of the first V2 epoch, and the block after the switch block), `getSnapshot` ignores any stored snapshot and rebuilds the pool from the `extraData` of the V1 switch block: the list in V1 order. This is the bootstrap gap rule of xOneGo PR 696. A pool built from the V1-era state at that gap gives other lists (the vector for Apothem 56,829,393 shows it).

**The lists of a switch header** (`calcMasternodes`). With `max` = `maxMasternodes` of the config at the round of the block, and `pool` the snapshot's candidates:

- block `switchBlock + 1`: `Validators` = `pool[:max]`, `Penalties` empty;
- an empty pool is an error;
- otherwise `Penalties` come from the V2 penalty engine (XB-10) and `Validators` = `pool` minus the penalties, in pool order, then cut at `max`. The cut is after the removal: a penalised candidate inside the top `max` is replaced by the next one. (V1 cuts first and then removes.)

A pool that is all penalised gives an empty list; the header check then rejects (`EmptyEpochSwitchValidators` if the header is empty, else `ValidatorsNotLegit`).

**Standby pool** (`GetStandbynodes`, used by the tiered reward): the pool of the epoch's gap snapshot, minus the epoch's masternodes and penalties, in pool order. Empty when the number of masternodes equals the number of candidates.

**Verification, no tolerant mode.** `HeaderVerifier::verify_derived` computes the two lists with the function it is given (`epoch_switch::expected`: gap snapshot and penalty engine). `Validators` must equal the computed list as a set, and from `ValidatorOrderBlock` in order; `Penalties` must equal the computed list as a set. An error from the computation rejects the block (`SwitchDerivation`); there is no fallback to the header's own lists, which geth keeps for bulk sync (`verifyHeader.go:236-319`, `!fullVerify`) and which a follower must not have. `HeaderVerifier::verify` with a `V2Context` compares against lists the caller derived, as before.

## Source

- `engine_v2/engine.go:1952-2013` `IsEpochSwitch`; `engine_v2/timeout.go:490-508` `isEpochSwitchAtRound`.
- `engine_v2/engine.go:915-931` gap and checkpoint number; `:947`, `:1110`, `:1145` the switch block's stored snapshot is ignored; `:1379-1416` the rebuild from the switch header (`:1397` `isSwitchBlock`, V1 `extraData` via `decodeMasternodesFromHeaderExtra`).
- `engine_v2/engine.go:2277-2340` `UpdateMasternodesFromHeader` (pool from state); `:2169-2190` `sortCandidatesByStake`; `common/sort/slice.go` the frozen sort; `core/state/statedb_utils.go:95-130` the 0x88 readers.
- `engine_v2/engine.go:2023-2100` `calcMasternodes`: `:2025` the config at the round, `:2030-2035` `snapNum--`, `:2051-2062` first V2 block, `:2095-2098` removal then cut; `:3324` `removeItemFromArray`; `:3351-3375` `GetStandbynodes`.
- `engine_v2/verifyHeader.go:219-460` the comparison; `:322` set; `:412` order; `:423` penalties.

## Rust

`xone_consensus::v2::{epoch_switch::{is_epoch_switch, of_header, is_epoch_switch_at_round, lists, expected, SwitchQuery, Expected}, gap_snapshot::{gap_number, uses_v1_list, build, bootstrap, standby, SnapshotV2}, masternodes::{max_masternodes, calc, first_v2_block}}`; `xone_consensus::verify::{HeaderVerifier::verify_derived, Derive}`; readers `xone_state::system_contracts::validator_0x88`.

## Vectors

- `conformance/vectors/v2/gap-apothem.json` (100 switches) and `gap-net2040.json` (20): inputs are the 0x88 words of the gap block (`eth_getProof` of the length slot, the array slots and the cap slots at the gap block), expected values are the `validators` and `penalties` of the switch header, which the builder also checks against `XDPoS_getMasternodesByNumber`. The sample is every switch of the XB-10 penalty vectors (30 and 7), every `v2_epoch` block of the consensus corpus (20 and 10), and seeded random heights. For the 37 switches that have a penalty window the penalties are computed by the penalty engine from that window; for the rest the header's penalties are the input and the pool, the removal, the cap and the order are checked. One switch of each network is in the first V2 epoch (bootstrap rule), and one bootstrap vector per network is the first V2 block. Built by `conformance/vectors/v2/build/gap.py`.
- `sort.json`, `calc.json`, `epoch.json`, `midadd.json`: pinned geth (`oracle/v2` commands `sort`, `calc`, `epoch`, `midadd`): 30 sort cases up to 464 candidates in three stake distributions with a hole in the array; 19 `calcMasternodes` cases (cap binds at 464 with and without penalties, a cap that changes with the round, a penalty outside the pool, an all-penalised pool, an empty pool, the first V2 block, the first epoch); 638 detection cases.
- Side branches: the archive serves one block per height and no non-canonical sibling of 85,209,326 or 85,209,327 (`debug_getBadBlocks` lists only heights from 85,225,457, `eth_getUncleCountByBlockNumber` is 0, re-checked for XB-11). The follower rules here take every header by hash through a trait (`commit::Headers`), so a sibling at the same height cannot shadow the block a hash names; `tests/v2_vectors.rs` has a synthetic two-branch case for the commit walk. Header lookup on a side branch is a storage rule (XB-07).

## Not covered

- The sequential replay of Apothem 56,828,700 to 61,289,999 with at least 1,000 switches belongs to the replay runner (XA-05).
- The pool when the gap block's state is not held (snap pivot, `ErrV2SnapshotMissing`, the SYNC-11 and forward-seed paths of this fork): typed `Missing`; the driver chooses (XB-13).
