# XDPoS V1 double validation

## Rule

Every V1 block past the first epoch (`number > epoch`) has two signatures over one digest. The creator (M1) signs in the last 65 bytes of `extraData`. The validator (M2) signs in the `validator` field. The digest is `SealHash`, which leaves out the last 65 bytes of `extraData`, `validators`, `validator` and `penalties` (geth `HashNoValidator` is the same value for V1).

1. `validator` is not empty (`NoValidatorSignature`).
2. It is 65 bytes and recovers an address over the seal hash (`InvalidValidatorSignature`).
3. That address is the M2 the checkpoint assigns to the creator (`FailedDoubleValidation`).

The assignment (`assigned_validator`, `m1_m2`):

1. The checkpoint is the block `number - number % epoch`. In the first epoch there is none: the assignment is the zero address. For a block that is itself a checkpoint, the block is read; for any other block the checkpoint header must be given (`MissingCheckpointHeader`). Its number must be a multiple of 900 (`M2Assignment`).
2. `masternodes` are the addresses in the checkpoint's `extraData`; `tokens` are its `validators` field read as 4-byte ASCII decimal tokens, NUL padding trimmed. One bad token empties the list. Fewer tokens than masternodes is `M2Assignment`.
3. With `m = len(masternodes)`, `moveM2 = ((number % epoch) / m) % m` from `TIPRandomize`, else 0. Masternode `i` gets `masternodes[(tokens[i] % m + moveM2) % m]`. A later duplicate masternode overwrites an earlier one. A remainder below zero wraps as an unsigned integer.
4. The creator's M2 is that map's entry; a creator that is not a masternode of the checkpoint gets the zero address.

geth turns the double validation off on Apothem (chain 51) only (`v1DoubleValidation`, `xdpos.go:1371`). Apothem blocks therefore need not satisfy it, and the vectors count how many do. The rule is tested through the negative pairs, which are made from Apothem blocks that do satisfy it.

The V1 timestamp check belongs with these creator checks: a block is at least `XDPoS.period` seconds after its parent, with the period of the genesis. No constant stands in for it (xOneGo #682; `v1::check_timestamp`).

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/xdpos.go:1347-1363` the checks in `verifySeal`; `:1371-1373` `v1DoubleValidation`; `:2233-2249` `RecoverValidator`; `:193-220` `sigHash`, `SealHash`; `core/types/block.go:162` `HashNoValidator`.
- `consensus/XDPoS/double_validation.go:162-180` `getM1M2`; `:184-195` `getM1M2FromCheckpointHeader`; `:141-152` the masternodes of a checkpoint's `extraData`.
- `consensus/XDPoS/xdpos_api.go:85-108` `GetValidator`; `:126-193` `getValidatorWithParents`.
- `consensus/XDPoS/xdpos.go:2732-2745` `ExtractValidatorsFromBytes`; `consensus/XDPoS/utils/constants.go:29` `M2ByteLength`; `consensus/XDPoS/constants.go:38` `EpocBlockRandomize`.
- `xdpos.go:981` the timestamp check; `xdpos.go:490-500` `New` (an unset `Period` becomes the engine default).

## The creator checks (`turn.rs`)

- Difficulty: `len(masternodes of the parent's epoch) - Hop(len, pre, cur)`, with `pre` the position of the parent's creator (-1 for the genesis parent) and `cur` that of the creator (-1 if absent). The in-turn creator carries `len`. geth skips the comparison when its `YourTurn` fails (no snapshot, no masternodes, a parent whose seal does not recover); here each of those is a typed error (`MissingMasternodes`, `MissingParentCreator`, `Seal`) and the comparison always runs (`xdpos.go:1261-1272`, `:1875-1881`, `:1983-2011`).
- Authorised: the creator is in the snapshot's signers, or else in the masternodes of the block (`Unauthorized`; `:1296-1309`).
- With more than one masternode, outside a checkpoint, the creator is not the creator of the parent (`Unauthorized`; `:1311-1322`). The newest entry of the snapshot's `recents` is the parent's creator, and only that entry is within two blocks.

## Networks

Every XDC network with a V1 phase; on Apothem the double validation is not enforced by geth.

## Rust

- `xone_consensus::v1::double_validation`: `m2_tokens`, `m1_m2`, `assigned_validator`, `check_double_validation`; `turn`: `hop`, `in_turn`, `difficulty`, `check_difficulty`, `authorize`, `out_of_turn_wait`; `check_timestamp`. `HeaderVerifier` calls these.
- Vectors: `conformance/vectors/consensus/v1/m1m2.jsonl`, Apothem V1 blocks (76 epochs, one per stratum, 4 blocks each; the first epoch and the epoch of the switch block always; the count is limited by the archive's 5 requests per second, see the README of the vectors). Each holds the header, its parent and checkpoint, and what the pinned geth computes: the creator (`Author`), the validator (`RecoverValidator`), the assigned M2 (`GetValidator`) and the seal hash. The test requires all four to be equal, the difficulty of the archive's header to equal `difficulty(...)`, and `authorize` to accept the creator.
- Negative pairs: `v1_wrong_m2`, `v1_validator_missing`, `v1_validator_unrecoverable`, `v1_creator_not_authorised`, `v1_creator_signed_the_parent`, `v1_difficulty_out_of_turn_claims_in_turn`, `v1_difficulty_in_turn_claims_out_of_turn`.
- `v1_negative.rs`, `the_v1_timestamp_check_uses_the_genesis_period`: a fast-period spec (xOneGo #682).

## Not covered

- The producer side: `Seal`, `CoSignM2Header` (`xdpos_api.go:20-76`) and the wait of an out-of-turn masternode are not rules of a follower; `out_of_turn_wait` is only the formula.
- The `Validators` field of a checkpoint is not derived (it needs the 0x90 randomize values).
