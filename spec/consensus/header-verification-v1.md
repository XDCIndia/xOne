# XDPoS V1 header verification

## Rule

An accepting node checks a V1 header (number at or below the V2 switch block) in this order and stops at the first failure. There is no "light" or "bulk-sync" variant here; where geth-XDC has one, this page lists the strict branch.

1. Number 0 is not verified by this rule set (`Genesis`).
2. Optional tail: before Amsterdam, none of `withdrawalsHash`, `blobGasUsed`, `excessBlobGas`, `parentBeaconRoot`, `requestsHash`, `blockAccessListHash`, `slotNumber` may be set. From Amsterdam, `blockAccessListHash` must be set, `withdrawalsHash` is the empty-list root, `blobGasUsed` and `excessBlobGas` are 0, `parentBeaconRoot` is zero, `requestsHash` is the SHA-256 of nothing, `slotNumber` is unset (`UnsealedFields`). `baseFee` is not restricted here.
3. `time` is not after the caller's clock (`FutureBlock`).
4. A checkpoint block (`number % epoch == 0`) has a zero coinbase (`CheckpointBeneficiary`).
5. The nonce is all zero or all ones (`InvalidVote`); on a checkpoint it is all zero (`InvalidCheckpointVote`).
6. `extraData` is at least 32 bytes (`MissingVanity`) and at least 32 + 65 bytes (`MissingSignature`).
7. Between vanity and seal there is a signer list only on a checkpoint block (`ExtraSigners`), unless the block is at or above the switch block; on a checkpoint its length is a multiple of 20 (`InvalidCheckpointSigners`).
8. The mix digest is zero (`InvalidMixDigest`); the uncle hash is the hash of the empty list (`InvalidUncleHash`).
9. Parent: the supplied parent has number `n - 1` and the header's parent hash (`UnknownAncestor`). `parent.time + period <= time` (`InvalidTimestamp`).
10. Checkpoint signer list (not on chain 51): the addresses in `extraData` equal, as a multiset, the signers the caller expects for this checkpoint (`InvalidCheckpointSigners`). The caller's set is the snapshot's signers minus the penalties of this header and of the four checkpoints before it (`v1::checkpoint::checkpoint_signers`): a penalised masternode is in the snapshot and not in the list (mainnet 469,800).
11. Seal: the signer is recovered from the last 65 bytes of `extraData` over the seal hash.
    - Difficulty: `len(masternodes at parent) - Hop(len, index of the parent's creator, index of the signer)`, compared with the header unless that value is 0, which is where geth's `YourTurn` fails and the comparison is skipped (`InvalidDifficulty`).
    - The signer is in the snapshot signers at the parent or in the masternodes of the block (`Unauthorized`).
    - With more than one masternode, outside a checkpoint, the signer is not the creator of the parent (`Unauthorized`).
12. Double validation (every chain but 51, `number > epoch`): `validator` is not empty (`NoValidatorSignature`), recovers over the same seal hash (`InvalidValidatorSignature`) and is the M2 assigned to the creator by the checkpoint of the epoch (`FailedDoubleValidation`). The M2 map sends masternode `i` of the checkpoint's `extraData` to masternode `(token[i] % N + move) % N`, where `token` is the 4-byte ASCII decimal list in the checkpoint's `validators` field and `move = ((number % epoch) / N) % N` from TIPRandomize, else 0.

V1 has no gas limit, gas used or base fee check (the comment at `xdpos.go:879-886` records that canonical V1 has none and that adding one would reject canonical history).

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/xdpos.go:808` `verifyHeader`; `:812` unsealed fields; `:826` dispatch to V2 for `number > switchBlock`.
- `:766-804` `verifyNoUnsealedFields`.
- `:833` future block; `:838-840` checkpoint beneficiary; `:843-848` nonce; `:851-856` vanity and signature length; `:859-867` signer list; `:871` mix digest; `:876` uncle hash.
- `:958` `verifyCascadingFields`: `:961` genesis; `:972` parent; `:981` timestamp; `:996-1019` checkpoint signer list (`v1FullVerifyChain`, `:1210`, is false on chain 51); `:1022` seal.
- `:1028-1130` `checkSignersOnCheckpoint`, list compare at `:1122-1125` (`compareSignersLists`, `:2687`, is a multiset compare).
- `:1235` `verifySeal`: `:1249` recover; `:1261-1270` difficulty; `:1294-1308` authorised; `:1312-1320` recent signers; `:1347-1362` double validation; `:1371` `v1DoubleValidation` (false on chain 51).
- `:1875` `calcDifficulty`, `:1983` `YourTurn`, `:2658` `Hop` (and `utils/utils.go:29`), `:2648` `position`.
- `consensus/XDPoS/double_validation.go:65` `getM1M2`, `:87` `getM1M2FromCheckpointHeader`; `consensus/XDPoS/xdpos_api.go:126` `getValidatorWithParents`; `consensus/XDPoS/xdpos.go:2732` `ExtractValidatorsFromBytes`.
- `consensus/XDPoS/xdpos.go:220` `encodeSigHeader` (the seal hash); `core/types/gen_header_rlp.go` (field order).

## Networks

Apothem (51) and every other XDC network with a V1 phase. On chain 51 steps 10 and 12 do not run, as in geth.

## Rust

- `xone_consensus::verify::HeaderVerifier::verify` with a `Context::V1(V1Context)` (`crates/xone-consensus/src/verify/header.rs`, `verify_v1`, `verify_seal_v1`). The timestamp, difficulty, creator and double-validation rules are the pure functions of `xone_consensus::v1` (`check_timestamp`, `turn::difficulty`, `turn::authorize`, `double_validation::assigned_validator`, `check_double_validation`); see `v1-snapshot.md`, `v1-checkpoint-masternodes.md`, `v1-penalties.md` and `v1-double-validation.md`.
- Where geth skips the difficulty comparison because its `YourTurn` fails (no masternodes, a parent whose seal does not recover), the verifier returns a typed error (`MissingMasternodes`, `Seal`): a missing input is never a pass.
- Inputs the caller supplies, from the archive in the corpus: `masternodes` (`XDPoS_getMasternodes(n)`), `parent_masternodes` (`XDPoS_getMasternodes(n-1)`), `signers` (`XDPoS_getSignersAtHash(parent)`), the checkpoint header of the epoch, and for a checkpoint block `checkpoint_signers` (`XDPoS_getSignersAtHash(block)`).
- Typed errors: `HeaderError` in `crates/xone-consensus/src/verify/error.rs`; its variant names are the `expect` of a negative vector.
- Corpus: `crates/xone-consensus/tests/corpus.rs`; rules with their recipes in `conformance/negative-gen/src/main.rs` (names starting `v1_` and the common ones).

## Not covered

- Checkpoint penalties (`errInvalidCheckpointPenalties`, `xdpos.go:1075`) and the removal of penalties of the previous epochs from the expected signers (`:1085`): the list is derived in `v1::checkpoint` and `v1::penalty` and tested against the archive's checkpoint headers (`v1-checkpoint-masternodes.md`, `v1-penalties.md`); only the comeback part is XB-10. `HeaderVerifier` still takes the final expected signer list from its caller, which the archive supplies as the snapshot at the checkpoint, so for a header that list is read from the same node that produced it.
- The ignore list `IgnoreSignerCheckBlocksMainnet` and its Apothem twin (`consensus/XDPoS/constants.go:162-184`) skips the checkpoint signer compare on a few blocks of chain 50 and 51; chain 51 skips it anyway and no mainnet vector exists.
- The recent-signers rule uses the creator of the parent. geth reads the snapshot's `Recents` map; the newest entry of that map is the parent's creator (`snapshot.go` apply), and only that entry can be within two blocks.
- Seal check order matters for the negative vectors: a V1 header is sealed over every field but the last 65 bytes of `extraData`, `validators`, `validator` and `penalties`, so a mutated field changes the recovered creator. A rule is therefore only testable if geth rejects before it recovers or the rule does not depend on the creator (see the recipes).
