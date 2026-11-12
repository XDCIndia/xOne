# XDPoS V1 vectors

Apothem (chain 51) V1 data from a geth-XDC archive and from the pinned geth-XDC. Read by `crates/xone-consensus/tests/v1_vectors.rs` and `v1_negative.rs`. Never edit a vector by hand: run `crates/xone-consensus/oracle/v1` (or `cargo run -p xone-negative-gen --bin xone-negative-gen-v1`) again. Every expected value is a header field or an RPC answer of the archive, or a value the pinned geth-XDC computed; none is typed by hand.

| file | lines | what | written by |
| --- | --- | --- | --- |
| `checkpoints.jsonl` | 600 | one Apothem checkpoint per line: the first 50 (900 .. 45,000), the last 50 up to the switch block 56,828,700, and 500 stratified over the rest (one random checkpoint per equal stratum) | `oracle/v1 checkpoints` |
| `m1m2.jsonl` | 76 (301 blocks) | one epoch per line: its checkpoint header and 4 random blocks of it (the first epoch and the epoch of the switch block always), with creator, validator and assigned M2 as the pinned geth computes them | `oracle/v1 m1m2` |
| `snapshot.jsonl` | 29 | windows of 40 consecutive headers (20 stratified, 8 over a checkpoint, and blocks 1350 to 1810) with `XDPoS_getSnapshot` before and after: 1,580 blocks | `oracle/v1 snapshot` |
| `snapvotes.jsonl` | 24 | synthetic chains of 99 headers with votes, epoch 50 (so the votes reset at block 50); the pinned geth's snapshot at 8 heights each (192 snapshots, 64 of them at or after block 50 with votes in flight) | `oracle/v1 snapvotes` |
| `gosort.jsonl` | 1,206 | inputs with many equal keys and the order the pinned geth's frozen sort leaves them in | `oracle/v1 gosort` |
| `negative.jsonl` | 66 | pairs (`accept`, `reject`) for 12 rules, made from the lines above | `xone-negative-gen-v1` |
| `mainnet_penalised.jsonl` | 10 | mainnet (chain 50) checkpoints with `Penalties` and the four before each (469,800 .. 475,200, 762,300, 3,880,800, 3,881,700): header RLP re-hashed against the RPC, penalties of the earlier checkpoints. Read by `v1_mainnet_penalised.rs`; not an Apothem vector | `gen_mainnet_penalised.py` (public RPC, `pycryptodome`) |

## Archive and pin

| | |
| --- | --- |
| host, port | xdc02, 127.0.0.1:21105 (read through an ssh tunnel to local 17105) |
| `web3_clientVersion` | `XDC/v1.17.10-xdc.20-08d25caf-20261028/linux-amd64/go1.25.0` |
| `eth_chainId` | `0x33` |
| head at export | 85,237,228 |
| export date | 2026-11-11 |
| seeds | 1 for every sample |
| oracle | the pinned geth-XDC (`conformance/PIN`, `6fc8e8c2088b7f18264f5663036669d04fa7becf`), built with Go 1.25.2 and `GOTOOLCHAIN=local`; the oracle's `go.mod` replaces geth with `conformance/geth-src` (`conformance/fetch-geth.sh`). The source used for this export was a copy at the pin path given for the task; it has no git metadata, so its commit could not be checked |

Requests are batches, at most one HTTP request every 220 ms (5 per second).

## Why `m1m2.jsonl` has 301 blocks

The plan was 500 epochs of 4 blocks (2,000 blocks). Each block needs five requests (header and parent in a batch, two masternode lists, the snapshot signers), and the archive allows 5 requests per second, so 2,000 blocks take longer than the time boxed for this slice. The sample was cut to 76 epochs, one per equal stratum of the 63,144 V1 epochs, 4 blocks each (the epoch of the switch block has one): 301 blocks. The test asserts at least 300. A larger sample is `oracle/v1 m1m2 -strata 500 -per 4` and a longer run; the same test reads it unchanged. The number of checkpoints (600) and snapshot windows (29) are as planned; the gate of 1,000 mainnet checkpoints and 10,000 sampled blocks per network of #29 is not met here (see below).

## Commands

```sh
ssh -N -L 127.0.0.1:17105:127.0.0.1:21105 xdc02 &
sh conformance/fetch-geth.sh            # conformance/geth-src at the pin
cd crates/xone-consensus/oracle/v1      # needs GOTOOLCHAIN=local GOFLAGS=-mod=mod
go run . checkpoints -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/checkpoints.jsonl
go run . m1m2 -strata 76 -per 4 -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/m1m2.jsonl
go run . snapshot -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/snapshot.jsonl
go run . snapvotes -out ../../../../conformance/vectors/consensus/v1/snapvotes.jsonl     # no RPC
go run . gosort -out ../../../../conformance/vectors/consensus/v1/gosort.jsonl           # no RPC
cd ../../../.. && cargo run -p xone-negative-gen --bin xone-negative-gen-v1
```

`checkpoints` and `m1m2` resume: a number already in the output file is skipped.

## Line formats

`checkpoints.jsonl` (Rust shape: `Cp` in `v1_vectors.rs`)

- `number`, `hash`; `extra`: the masternodes in the `extraData` of the header; `penalties`: its `Penalties` field.
- `tipSigning`, `tipRandomize`: the pinned geth's `IsTIPSigning` and `IsTIPRandomize` at `number`. The test requires the chain spec to agree.
- `prevMasternodes`: the list in the previous checkpoint (the genesis `extraData` for block 900). `prevPenalties`: `Penalties` of the checkpoints `number - i * 900`, `i = 1..4`, for each with `number > i * 900`. `comeback`: `Penalties` of the checkpoint `number - 4500`, when `number > 4500`.
- `gapNumber` (`number - 450`); `candidateWords`: `[slot, value]` pairs of the 0x88 contract in the state after the gap block (`eth_getStorageAt`): the length of the candidates array, each element, and `validatorsState[candidate].cap` of each non-empty element. The slots are computed by the pinned geth's `GetLocDynamicArrAtElement` and `GetLocMappingAtKey`; the test reads them with the `xone-state` readers (`validator_0x88::candidates`, `candidate_cap`).
- `snapshotGap`: signers of `XDPoS_getSnapshot(gapNumber)`.
- Before TIPSigning: `signerBlocks` (the counted blocks `i` in `[number - 900, number)` with `i % 15 == 0` or `i < TIP2019`, with their hashes) and `signerWords` (the 0x89 words of those blocks in the state after `number - 1`; read with `signer_0x89::signers`).
- From TIPRandomize: `creators`, `[address, count]`: the creators the pinned geth recovers (`RecoverSigner`) over the blocks `number - 899 .. number - 1`.

`m1m2.jsonl`: `epoch`, `checkpoint` (`rlp(header)`, null for epoch 0), `blocks[]`: `number`, `hash`, `header` and `parent` (`rlp`), `sealHash` (`SealHash`), `creator` (`RecoverSigner`), `validator` (`RecoverValidator`, null if absent or not recoverable, with `validatorError`), `assigned` (`GetValidator`, or `assignedError`), `masternodes` (`XDPoS_getMasternodes(number)`), `parentMasternodes` (`... (number - 1)`), `signers` (`XDPoS_getSigners(number - 1)`).

`snapshot.jsonl`: `start`, `end` (the JSON of `XDPoS_getSnapshot` at the first block - 1 and the last block), `headers` (`rlp`), `why`. `snapvotes.jsonl`: `genesis`, `headers` (blocks 1 to 99), `at`, `snapshots` (JSON of the pinned geth's `Snapshot`). `gosort.jsonl`: `caps`, `order` (original indices after the sort). `negative.jsonl`: the shape in `conformance/negative-gen/src/v1case.rs`.

## How the penalty list is derived and checked

The penalty list of a checkpoint is not an archive field of its own; it is derived from the archive's data by the rule geth picks by block number (`spec/consensus/v1-penalties.md`).

- Before TIPSigning (`HookPenalty`): the counts are the signers listed in the 0x89 state after `number - 1` for the counted blocks (`signerBlocks`, `signerWords`).
- From TIPSigning with TIPRandomize (`HookPenaltyTIPSigning`): the counts are the creators of blocks `number - 899 .. number - 1` (`creators`). No state is read.
- Between TIPSigning and TIPRandomize the rule returns only comebacks, which are XB-10.
- `penalties_v1` returns the previous checkpoint's masternodes with no count. The test requires it to be a prefix of the header's `Penalties`. What follows must be a subsequence of `comeback.penalties` restricted to `snapshotGap` (a comeback: penalised five epochs ago and a candidate again); before TIPSigning nothing may follow. The comeback part itself (sign transactions of the last 150 blocks) is not derived: it is XB-10.
- The list in `extraData` is then `checkpoint_masternodes(candidates, max, removed)` with `removed` = the header's `Penalties` plus the `Penalties` of the four earlier checkpoints, compared with `extra` in order.

## Results

| check | result |
| --- | --- |
| `extra` equals the derived list, 600 checkpoints | 600 equal, 0 differ (76 before TIPSigning, 520 with creators, 4 comeback-only; 11 with a penalty from the rule; 93 with comebacks) |
| kept candidates equal `XDPoS_getSnapshot(gap)` | 600 of 600; at most 15 candidates ever, so the cap never cut a list |
| creator, validator, assigned M2, seal hash equal the pinned geth | 301 of 301 blocks; 297 past the first epoch, all with validator equal to the assigned M2; 4 without a validator (first epoch) |
| difficulty and creator rules on the archive's lists | 301 of 301 (5 blocks out of turn) |
| `apply` equals the archive snapshot | 29 of 29 windows, 1,580 blocks, 10 over a checkpoint |
| `apply` equals the pinned geth with votes | 192 of 192 snapshots (votes in flight, changed signer sets, the reset at block 50) |
| frozen sort equals the pinned geth | 1,206 of 1,206 (1,147 with equal keys) |
| negative pairs | 66 pairs, 12 rules; the unmutated twin is accepted and the mutated one rejected with the typed error |

## Not covered

- The other 62,543 Apothem checkpoints, and all mainnet checkpoints and blocks.
- M2 and M1 for 10,000 sampled blocks per network (301 Apothem blocks here).
- The comeback and sticky part of the penalties and the sign-transaction scan: XB-10.
- The `Validators` field of a checkpoint (0x90 randomize values).
- Apothem does not enforce the checkpoint list or the double validation in geth; these vectors test the derivation, not an enforcement on that chain.
