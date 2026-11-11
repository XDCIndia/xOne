# XDPoS V1 penalties

## Rule

At a V1 checkpoint, the masternodes of the previous epoch that did not sign enough are penalised: they leave the new list (`v1-checkpoint-masternodes.md`) and their address is written to the checkpoint's `Penalties` field, which removes them again from the next four lists. geth picks the rule by the checkpoint's number (`v1CheckpointPenalties`). Below `epoch` there are no penalties.

`PenaltyEra::BlockSigners` (before `TIPSigning`; `HookPenalty`):

1. The candidates are the masternodes in the previous checkpoint's `extraData`.
2. The counted blocks are `i` in `[number - epoch, number)` with `i % MergeSignRange == 0` or `i < TIP2019` (`scan_blocks`). `MergeSignRange` is 15.
3. For each counted block, the 0x89 contract state lists the masternodes that signed it, by block hash. The state is the one the head had when the checkpoint was built: the state after `number - 1`.
4. A candidate with no count is penalised.

`PenaltyEra::Creators` (from `TIPSigning` and `TIPRandomize`; `HookPenaltyTIPSigning`):

1. The candidates are the same.
2. The counted blocks are `number - 1` down to `number - (epoch - 1)`, and a block counts for the address that created it (the seal). No state is read.
3. A candidate that created no block is penalised. (geth also lists a non-candidate miner below `MinimunMinerBlockPerEpoch`, which is 1, so nobody is below it.)

`PenaltyEra::ComebackOnly` (from `TIPSigning`, below `TIPRandomize`): geth returns only the comeback list, so `penalties_v1` returns nothing.

`penalties_v1(era, prev_masternodes, sign_counts)` returns the candidates with fewer than `MinimunMinerBlockPerEpoch` counted blocks, in the order of `prev_masternodes`. A missing masternode list is `MissingMasternodes`. No counted block at all is `MissingSignData`: scanning nothing would penalise everyone, which is the erigon defect `64f97f9b75` turned upside down. The counts are scanned in ascending block order by the caller (`scan_blocks`), never newest first (erigon `61baa21489`).

The comeback and sticky part is XB-10. Both hooks end with it: a masternode that was penalised `LimitPenaltyEpoch + 1` epochs ago (a `Penalties` entry of the checkpoint `number - 5 * epoch`) and is a candidate again is penalised again unless it sent a sign transaction for a block `% MergeSignRange == 0` among the last `RangeReturnSigner` (150) blocks (`hooks.go:242-297`). It needs the block bodies, so a missing body will be a typed error there. From `TIPRandomize` the penalties are the rule's list followed by the comebacks; below it, the comebacks alone (`hooks.go:297-301`).

`verify_checkpoint_penalties(header_bytes, computed)` is geth's byte compare of the header's `Penalties` with the computed list (`errInvalidCheckpointPenalties`).

### How the vectors check the rule without the comeback part

For each of the 600 checkpoints the test computes `penalties_v1` from the archive's data and requires it to be a prefix of the checkpoint's `Penalties`. What follows the prefix must be a subsequence of the `Penalties` of the checkpoint five epochs back, restricted to the candidates of the gap block: that is what a comeback is, and nothing else may be there. Before `TIPSigning` the header's `Penalties` must equal the rule's list exactly. The list in `extraData` is then derived with the header's own `Penalties` and the four earlier ones, so a comeback is the one input taken from the header.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/xdpos.go:1215-1231` `v1CheckpointPenalties`; `:1054-1077` the compare; `:1440-1466` `Prepare`.
- `eth/hooks/hooks.go:124-176` `HookPenalty`: `:146` candidates, `:149` counted blocks, `:158` the 0x89 read, `:135` the head state. `core/state/statedb_utils.go:22-40` `GetSigners`.
- `eth/hooks/hooks.go:182-302` `HookPenaltyTIPSigning`: `:205-224` creators of `number - 1 ..= number - (epoch - 1)`, `:230-240` the list, `:242-297` comebacks, `:297-301` the return.
- `consensus/XDPoS/constants.go:44` `LimitPenaltyEpoch`, `:56-57` `MergeSignRange`, `RangeReturnSigner`, `:60` `MinimunMinerBlockPerEpoch`.
- `consensus/XDPoS/penalty.go:44-76` `CalculatePenalties` is a helper nothing in V1 calls.

## Networks

Apothem is tested; `TIP2019` is block 1, `TIPSigning` 3,000,000, `TIPRandomize` 3,464,000 (`network_constants.go:77-79`).

## Rust

- `xone_consensus::v1::penalty`: `penalty_era`, `scan_blocks`, `penalties_v1`; `checkpoint::checkpoint_penalties`, `verify_checkpoint_penalties`.
- Vectors: the `checkpoints.jsonl` lines hold, by era, the 0x89 storage words of the counted blocks of the previous epoch (`signerWords`, read at the state after `number - 1`) or the creator counts the pinned geth recovered (`creators`).

## Not covered

- Comebacks, sticky penalties and the sign-transaction scan: XB-10.
- V2 penalties (`engine_v2`).
