# XDPoS V1 checkpoint masternode list

## Rule

A V1 checkpoint (`number % epoch == 0`, `number >= epoch`) carries the masternodes of the next epoch in `extraData`, between the 32-byte vanity and the 65-byte seal. The list is made in two steps, at two blocks.

At the gap block (`number % epoch == epoch - gap`):

1. Read every candidate of the 0x88 validator contract with its cap, in the state after the gap block. An empty address entry is skipped (`validator_0x88::candidates`, `candidate_cap`). No candidate is `MissingCandidates` (geth refuses an empty set).
2. Sort by cap, largest first, with geth's frozen sort and its comparator `cap(i) >= cap(j)`. The sort is an unstable quicksort, so the order of equal caps is part of the rule: `xone_consensus::v1::gosort` repeats it step by step and is tested against the pinned geth on 1,206 inputs with many equal keys.
3. Keep the first `maxMasternodes`: 18, or `maxMasternodesV2` once the gap block number is at or above `TIPIncreaseMasternodes` or above the V2 switch block. The chain spec carries neither value for Apothem (the fork row is unset and `XDPoS.maxMasternodesV2` is 0), so geth reads the network constants: 5,000,000 and 15 on Apothem.
4. This set is the snapshot's signer set (`Snapshot::reseed`).

At the checkpoint:

5. Start from the snapshot's signers, which `get_signers` gives ascending by address bytes.
6. Remove the penalties of this epoch (`v1-penalties.md`).
7. Remove the penalties kept in the `Penalties` field of the checkpoint headers `number - i * epoch`, `i = 1..4` (`LimitPenaltyEpoch`), for each with `number > i * epoch`. A checkpoint header that is not there is `MissingPenaltyHeader` (geth skips it).
8. The rest, in that order, is the list. Nothing left is `EmptyCheckpointList`.

So the cap decides who is in; the order in the header is by address. `checkpoint_masternodes(candidates_with_caps, max_masternodes, penalties)` is steps 1 to 8; `penalties` is `checkpoint_penalties(...)`.

Verification of a header compares the list as a multiset (geth `compareSignersLists`): a dropped or an extra address is `InvalidCheckpointSigners`. `HeaderVerifier` keeps that rule. `verify_checkpoint_list(listed, expected)` is stricter: the same set in another order is `CheckpointOrder`, because the producer rule above fixes the order. `HeaderVerifier` does not call it: geth accepts any order, and a stricter rule there would reject blocks the network accepts.

Apothem (chain 51) runs none of the checkpoint list checks in geth (`v1FullVerifyChain`, `xdpos.go:1210`); the vectors below test the derivation against the archive's headers, not an enforcement.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/xdpos.go:2135-2152` `UpdateMasternodesV1`: `:2137-2139` candidates and caps, `:2143` empty set, `:2148-2150` sort. `common/sort/slice.go:29-190` `Slice`.
- `:2156-2177` `UpdateMasternodes`: `:2163-2174` the limit, `:2175-2177` the cut, `:2185-2189` the signer set.
- `eth/hooks/hooks.go:307-346` `HookGetSignersFromContract` and `signersFromState` (the same sort and the 150 cut of the contract fallback, `:335-340`); `core/state/statedb_utils.go:95-112` `GetCandidates`, `:123-130` `GetCandidateCap`.
- `consensus/XDPoS/snapshot.go:249-263` `GetSigners`.
- `xdpos.go:1386-1495` `Prepare`: `:1439` the signers, `:1440-1466` this epoch's penalties, `:1468-1472` the previous epochs', `:1474-1476` the list written. `:2618-2637` `removePenaltiesFromBlock`; `:2703-2718` `removeItemFromArray`.
- `xdpos.go:1028-1139` `checkSignersOnCheckpoint`, `:1125` the multiset compare, `:2687-2701` `compareSignersLists`; `:1149-1173` `checkpointSigners` (the gap snapshot) and `:1181-1205` `getSignersFromContract`.
- `consensus/XDPoS/constants.go:41` `MaxMasternodes`, `:44` `LimitPenaltyEpoch`; `consensus/XDPoS/network_constants.go:85` and `:100` (Apothem).
- `xdpos.go:2122-2127` `GetMasternodesFromCheckpointHeader` (the list is read back from `extraData`).

## Networks

Apothem is tested. Mainnet differs in the constants (`maxMasternodesV2` 108) and has four ignore-listed checkpoints (`consensus/XDPoS/constants.go:162-184`); no mainnet vector exists yet.

## Rust

- `xone_consensus::v1::checkpoint`: `select_candidates`, `max_masternodes`, `checkpoint_masternodes`, `checkpoint_penalties`, `verify_checkpoint_list`, `masternodes_of`.
- Vectors: `conformance/vectors/consensus/v1/checkpoints.jsonl`, 600 Apothem checkpoints: the first 50, the last 50 up to the switch block 56,828,700 and 500 stratified over the rest. Each line holds the archive's header list and penalties, the 0x88 storage words of the gap block (read with `eth_getStorageAt`, slots computed by the pinned geth), the archive's snapshot at the gap block and the inputs of the penalty rule. The test reads the words with the `xone-state` readers and requires `checkpoint_masternodes` to equal the header's list, in order, for every checkpoint, and the kept candidates to equal the snapshot at the gap block. Test: `v1_vectors.rs`, `every_checkpoint_list_equals_the_one_derived_from_the_candidates`.
- Negative vectors: `v1_checkpoint_list_dropped`, `_extra`, `_reordered` in `negative.jsonl`.

## Not covered

- The list of a checkpoint whose penalties include comebacks is checked with the header's own `Penalties` field as input (`v1-penalties.md`).
- The `Validators` field (the M2 list of the checkpoint, from the 0x90 contract; `eth/hooks/hooks.go:348-371`) is read by `double_validation`, not derived here; it is not enforced on Apothem.
- Mainnet and the ignore list.
