# Consensus vectors

Whole blocks from two geth-XDC archive nodes, and the invalid blocks made from them. Read by `crates/xone-consensus/tests/corpus.rs`. Never edit a vector by hand: run the exporter or the generator again.

| path | what | written by |
| --- | --- | --- |
| `valid/apothem.jsonl` | 300 Apothem (chain 51) blocks | `conformance/exporter blocks` |
| `valid/net2040.jsonl` | 100 net2040 (chain 2040) blocks | `conformance/exporter blocks` |
| `net2040.forks.json` | the fork table of net2040, in the format of `crates/xone-chainspec/generated/forks.json` | `conformance/exporter blocks -forks-out` |
| `negative/<rule>.jsonl` | the mutations of valid vectors, one file per rule | `cargo run -p xone-negative-gen` |

Every block is accepted by `HeaderVerifier` and `BodyVerifier`; every negative vector is rejected with exactly the error in its `mutation.expect`.

## Line format

One JSON object per line. The Rust shape is `conformance/negative-gen/src/vector.rs`.

- `id`, `network` (`apothem`, `net2040`), `class` (below), `number`, `hash` (the archive's block hash).
- `header`, `parent`: `rlp(header)` of the block and of its parent, from the archive's JSON re-encoded by the pinned geth-XDC types. The exporter stops if geth's `Header.Hash()` differs from the archive hash.
- `body.transactions`: the consensus encoding of each transaction (geth `Transaction.MarshalBinary`). `body.uncles`: `rlp(header)` of each uncle (none). `body.withdrawals`: the encoded withdrawals, `null` before Shanghai.
- `receipts`: `Receipt.MarshalBinary` of each receipt, from `eth_getBlockReceipts`. The exporter stops if geth's `DeriveSha` over the transactions, the receipts or the withdrawals differs from the header.
- `context`: what a verifier needs besides the block and its parent.
  - `consensus`: `v1` or `v2` (geth `IsV2Block`: above the switch block). `now`: the block's own time, so a valid block is never "from the future".
  - V2: `masternodes` and `penalties` (`XDPoS_getMasternodesByNumber(n)`), `parentMasternodes` (the same at `n-1`, absent when the parent is the V1 switch block, whose list is in its `extraData`), `parentEpochSwitch` (the block number of the epoch switch of the parent's epoch, the largest entry `<= n-1` of `XDPoS_getEpochNumbersBetween`; the V1 switch block for the first V2 block).
  - V1: `masternodes` and `parentMasternodes` (`XDPoS_getMasternodes(n)` and `(n-1)`), `signers` (`XDPoS_getSignersAtHash(parent)`), `checkpoint` (the header of the epoch's checkpoint, for the M2 assignment; absent on a checkpoint block), `checkpointSigners` (on a checkpoint block, `XDPoS_getSignersAtHash(block)`).
- `oracle`: what the pinned geth-XDC computes for the header: seal hash, V2 signature hash, `HashNoValidator`, the recovered signer.
- `notes`: anything the exporter could not do as asked.
- `mutation` (negative vectors only): `rule`, `stage` (`header` or `body`), `recipe` (what was changed), `expect` (the variant name of the typed error), `source` (the `id` it started from), and `activate` (forks switched on from block 0 for that vector, see below).

The masternode and penalty lists at an epoch switch are the archive's answer for the switch block, which for a switch block is the list of its own header. The corpus tests the comparison of the header with the list, not the derivation of the list from the 0x88 contract state (XB-11) or the penalties from the sign-transaction history (XB-10).

## Archive and export

| | Apothem | net2040 |
| --- | --- | --- |
| archive | geth-XDC `XDC/v1.17.10-xdc.20-08d25caf` (host xdc02, port 21105) | geth-XDC `XDC/net2040-geth-archive-de6c/v1.17.10-xdc.21` (host xdc02, port 10071) |
| head at export | 85,230,907 | 733,484 |
| export date | 2026-11-11 | 2026-11-11 |
| seed | 1 | 1 |

Requests are batches of 5, one batch per second.

```sh
ssh -N -L 127.0.0.1:17105:127.0.0.1:21105 -L 127.0.0.1:17071:127.0.0.1:10071 xdc02 &
cd conformance/exporter   # needs conformance/geth-src from conformance/fetch-geth.sh
go run . blocks -network net2040 -rpc http://127.0.0.1:17071 -out ../vectors/consensus/valid/net2040.jsonl \
  -forks-out ../vectors/consensus/net2040.forks.json -total 100 -v1-epochs 9 -v2-epochs 10 -v1 20 -sign 10
go run . blocks -network apothem -rpc http://127.0.0.1:17105 -out ../vectors/consensus/valid/apothem.jsonl \
  -total 300 -v1-epochs 20 -v2-epochs 20 -v1 50 -sign 40 -side 85209327
cd ../.. && cargo run -p xone-negative-gen
```

## Block list rule

A set of block numbers; the first rule that names a block gives its `class`, except that an ordinary block (`v1`, `v2`) that carries a transaction to `0x89` is relabelled `sign`, and a block drawn as a signing block that has none is relabelled `v2`.

1. `side`: the number named by `-side` (Apothem 85,209,327).
2. `v2_switch`: the V1 switch block and the first V2 block.
3. `v1_epoch`: the V1 checkpoints `switch - k * 900`, k = 1 to N.
4. `v2_epoch`: the first N/2 epoch-switch blocks after the V2 switch that `XDPoS_getEpochNumbersBetween` lists, then the first one at or after random heights, up to N.
5. `fork`: row-1, row, row+1 of `Eip1559Block`, `Gas2500xBlock`, `ValidatorOrderBlock`, `SigningTxBoundBlock`, `TIPSigningBlock`, `TIPRandomizeBlock`, `AmsterdamBlock`, `DynamicGasLimitBlock`, for rows that are scheduled and above the first epoch.
6. `sign`: random blocks of the signing-block spacing seen in the last 40 blocks below the head.
7. `v1`: random draws in (epoch, switch); then `v2`: random draws in [switch+2, head-1] until the list has `-total` blocks.

| class | Apothem | net2040 |
| --- | --- | --- |
| v1_epoch | 20 | 9 |
| v1 | 43 | 19 |
| v2_switch | 2 | 2 |
| v2_epoch | 20 | 10 |
| v2 | 182 | 34 |
| sign | 26 | 13 |
| fork | 6 | 13 |
| side | 1 | |
| total | 300 | 100 |

### The side-branch vector

85,209,327 on Apothem is a V2 epoch switch (97 masternodes, 35 penalties). The archive serves only the canonical block of a height (`0xfae98b2f...e0b8`); `debug_getBadBlocks` lists ten blocks at heights 85,225,457 and above and none here, and `eth_getUncleCountByBlockNumber` is 0. A sibling hash cannot be found from the archive, so the vector is the canonical block, with that said in its `notes`. A verifier is a pure function of the header and the parent it is given, so looking a header up by hash on a side branch is a storage rule, not a rule of this slice.

## Negative vectors

`negative/<rule>.jsonl` holds the mutations of one rule. Each starts from a valid vector, applies the recipe in `conformance/negative-gen/src/main.rs` (stored in `mutation.recipe`), re-encodes the header and writes the new hash. Per rule up to 6 Apothem and 4 net2040 vectors are chosen, evenly spread over the eligible ones. The expected error is written next to the recipe; it is never computed by running the verifier.

A signature covers the header, so a mutation of a signed field breaks the seal. A rule is tested only where geth rejects before it recovers the signer, or where the rule does not depend on the signer: that is why some recipes change the context (`v2_not_its_turn`, `v2_qc_gap_number_epoch_switch`) and why a QC mutation names the signature error (`v2_qc_gap_number_in_header`).

`ValidatorOrderBlock` and `SigningTxBoundBlock` are not scheduled on Apothem or net2040, so no valid block reaches their branches. Their two negative rules (`v2_validators_reordered`, `body_too_many_signing_txs`) carry `activate`, which switches the fork on from block 0 for that vector.
