# XDPoS V2 header verification

## Rule

An accepting node checks a header above the V2 switch block in this order, under geth's full verification (`fullVerify = true`), and stops at the first failure. Where geth has a tolerant branch for bulk sync, this page names the strict branch.

1. Number 0 is not verified (`Genesis`). The optional tail follows the same rule as V1 (`UnsealedFields`, see [header-verification-v1](header-verification-v1.md)).
2. `validator` is 65 bytes (`NoValidatorSignatureV2` when empty, `ValidatorSignatureLength` otherwise). The signature is not the V1 seal: it is over `v2_sig_hash`.
3. `time` is not after the caller's clock (`FutureBlock`).
4. Parent: number `n - 1` and the header's parent hash (`UnknownAncestor`).
5. Gas: `|parent.gasLimit - gasLimit| < parent.gasLimit / 1024` (`InvalidGasLimit`), `gasLimit >= 5000` (`GasLimitTooLow`), `gasUsed <= gasLimit` (`GasUsedExceedsLimit`).
6. `extraData` is the version byte 2 and exactly one RLP `ExtraFieldsV2` (`InvalidV2Extra`: wrong version, bad RLP, trailing bytes).
7. Timestamp: if the parent is above the switch block, `parent.time + minePeriod(round) <= time`, with the V2 config selected by the header's round (`InvalidTimestamp`). The first V2 block, whose parent is the switch block, is exempt.
8. `round > qc.proposedBlockInfo.round` (`RoundInvalid`).
9. Quorum certificate (below).
10. Nonce all zero or all ones (`InvalidVote`); mix digest zero (`InvalidMixDigest`); uncle hash the empty-list hash (`InvalidUncleHash`).
11. From `Eip1559Block` (not London): `baseFee` is set (`MissingBaseFee`) and equals 12.5 gwei, times 50 from `Gas2500xBlock` (`InvalidBaseFee`). Before `Eip1559Block` geth does not look at `baseFee` here, so a V2 header carrying one is accepted.
12. Difficulty is 1 (`InvalidDifficulty`).
13. Epoch switch: the QC's block is the V1 switch block, or `qc.round < round - round % epoch`.
    - Epoch switch: nonce all zero (`InvalidCheckpointVote`); `validators` not empty (`EmptyEpochSwitchValidators`) and a multiple of 20 bytes (`InvalidCheckpointSigners`); as a multiset equal to the masternodes of the epoch (`ValidatorsNotLegit`); from `ValidatorOrderBlock`, in the same order (`ValidatorsOrder`); `penalties` equal, as a multiset, to the penalties of the epoch (`PenaltiesNotLegit`; a trailing partial address is dropped before the compare, as in `ExtractAddressFromBytes`).
    - Any other block: `validators` and `penalties` are empty (`InvalidFieldInNonEpochSwitch`).
14. Signature: the masternode list of the block's epoch is not empty (`EmptyMasternodes`); `validator` recovers over `v2_sig_hash` and is in that list (`ValidatorNotWithinMasternodes`); it equals the coinbase (`CoinbaseAndValidatorMismatch`); it is the leader, `masternodes[round % epoch % len]` (`NotItsTurn`).

### Quorum certificate

The certificate is checked against the epoch of the parent, not of the block: at an epoch switch the new list signs nothing yet.

1. The masternodes of the parent's epoch. When the parent is the V1 switch block they are the address list in its `extraData`. The list is not empty and the QC's block hash is the parent's hash (`QcEpochInfo`).
2. Each distinct signature (byte-identical ones count once) recovers (`QcSignatureUnrecoverable`), has `s <= N/2` (`QcSignatureHighS`) and recovers to a masternode of that epoch (`QcSignerNotMasternode`). The signed digest is `keccak(rlp([[hash, round, number], gapNumber]))`. geth recovers signatures in parallel; with several bad signatures which error comes first is not defined, with one it is exact.
3. Signers are counted by recovered address. If the QC's round is above 0, the count must not be below `len(masternodes) * certificateThreshold`, compared in `float64` (`QcBelowThreshold`). The threshold is the V2 config selected by the QC's round. The smallest accepted count is `ceil(len * threshold)`. A QC of round 0 (the one in the first V2 block) has no signatures to count.
4. The QC's gap number is `epochSwitch - epochSwitch % epoch - gap`, or 0 if that is not above `gap`, where `epochSwitch` is the number of the epoch-switch block of the parent's epoch (`QcGapNumber`).
5. The QC's block number is the parent's (`QcBlockNumber`). If it is the switch block, its round is 0 (`QcSwitchRound`); otherwise it is the round in the parent's `extraData` (`QcRound`).

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/xdpos.go:808-826` entry and dispatch; `:766-804` unsealed fields; `consensus/XDPoS/engines/engine_v2/verifyHeader.go:68` `verifyHeader`.
- `verifyHeader.go:90-93` validator length; `:100` future; `:110-112` parent; `:117-131` gas; `:133` extra; `:143-151` timestamp; `:153` round; `:170` QC; `:177-186` nonce, mix, uncle; `:189-194` EIP-1559 (`consensus/misc/eip1559/eip1559.go:58-109`, `params/xdc_gas.go:36`, `params/protocol_params.go:175`); `:195` difficulty.
- `verifyHeader.go:219-460` epoch switch: `:225-232` nonce, empty, alignment; `:322` set compare (`consensus/XDPoS/utils/utils.go:59`); `:412` order (`utils.go:97`, hard failure from `IsValidatorOrder`); `:423` penalties; `:449-455` ordinary block; `:462-476` signature and coinbase; `:480-505` leader.
- `consensus/XDPoS/engines/engine_v2/engine.go:1952` `IsEpochSwitch`; `:2380` `getExtraFields`; `consensus/XDPoS/utils/utils.go:110` `DecodeBytesExtraFields` (version byte 2).
- `engine_v2/utils.go:55` `verifyMsgSignature`; `:86` `hasLowS`; `:131-262` `recoverUniqueSigners` (`:188` recover, `:201` low-S, `:211` not a masternode, `:221-227` byte duplicates).
- `engine_v2/utils.go:315-472` `verifyQC`: `:322` epoch info, `:329` digest (`core/types/consensus_v2.go:234`), `:345` signers, `:365-373` threshold, `:426-433` gap number, `:472` block info; `:619-668` `VerifyBlockInfo` (`:633` hash, `:638` number, `:646` switch round, `:661` round).
- `params/config.go:2425-2438` `V2.Config(round)`.
- Engine seal hash: `engine_v2/engine.go:285` `sigHash`.

## Networks

Every XDC network with a V2 phase.

## Rust

- `xone_consensus::verify::HeaderVerifier::verify` with `Context::V2(V2Context)` (`crates/xone-consensus/src/verify/header.rs`: `verify_v2`, `verify_qc`).
- `V2Context` is what the caller knows: `masternodes` and `penalties` of the block's epoch (at an epoch switch these are the lists the header must carry), `parent_masternodes` and `parent_epoch_switch` for the QC. In the corpus they come from `XDPoS_getMasternodesByNumber(n)` and `(n-1)`, and `XDPoS_getEpochNumbersBetween`.
- Chain parameters come from `xone_chainspec::ChainSpec`: `xdpos.epoch`, `xdpos.gap`, `xdpos.v2.config_at_round`, `is_eip1559`, `gas2500x`, `validator_order`, `amsterdam`.
- Corpus and recipes: `crates/xone-consensus/tests/corpus.rs`, `conformance/negative-gen/src/main.rs` (rules starting `v2_`).

## Where the Rust code is stricter than geth, and why

- Gap number: geth accepts a mismatch if the QC's gap number names a snapshot whose signer set equals the QC's epoch (`utils.go:440-466`, a bulk-sync repair for an epoch switch it could not resolve). Rust rejects it. A valid vector that needs the tolerance would fail and be listed; none does.
- `ValidatorOrderBlock` and `SigningTxBoundBlock` are unscheduled on Apothem, so no real vector reaches those branches. The negative vectors `v2_validators_reordered` and `body_too_many_signing_txs` switch the fork on from block 0 for that one vector (`mutation.activate`).

## Not covered

- The expected masternode list and penalties at an epoch switch come from the archive (`XDPoS_getMasternodesByNumber`). At a switch block the node answers with the list of the header itself, so the corpus tests the comparison, not the derivation. Deriving the list from the 0x88 state (`calcMasternodes`) and the penalties from the sign-transaction history is XB-10 and XB-11.
- Mutations that need a signature (a different QC round or number, a different coinbase, a different round with a valid seal) cannot be produced without validator keys. The signature covers the field, so such a mutation fails at the signature first, and the rules behind `QcBlockNumber`, `QcRound` and `QcSwitchRound` have no negative vector. The valid vectors cover their accept branch.
- The vote, timeout, TC and SyncInfo rules (V1-V9 of the checklist) are not header rules and are not in this slice.
