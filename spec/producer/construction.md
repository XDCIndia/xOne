# Block construction (XB-23)

## Rule

**Selection** (geth `fillTransactions`, `commitTransactions`). From the pool's executable set (`Pool::pending`):

1. The uninterrupted run of special transactions (to 0x89 or 0x90) at the front of each account's nonce queue goes first, outside the base fee check (a special transaction has gas price 0, below the base fee; it yields a zero tip). A special transaction behind a paid nonce stays with the paid ones, so nonce order holds.
2. The rest goes by effective tip (`min(feeCap - baseFee, tipCap)`) at the base fee of the block, then by time first seen, then by sender; each account's queue keeps its nonce order. A transaction whose fee cap is below the base fee drops its account.
3. The gas pool is the block's gas limit; a transaction that declares more than what is left skips its account, not the block; the loop ends when less than 21,000 is left, or when the used gas reaches `--miner.blockvotegascap` (soft: the last transaction may pass it). The soft cap itself is `geth` branch work at the pin and is a parameter here, default off.
4. A sign transaction with calldata shorter than 68 bytes, or one that signs a block number at or above the block being built, is skipped; at most `2 * maxMasternodesV2` (256 when unset) go in one block.
5. The caller's `commit` is geth's `commitTransaction`: `Used(gas)` shifts to the sender's next, `NonceTooLow` shifts, anything else drops the sender.

Order is not consensus: a valid producer may order differently. **Given the order, the bytes are fixed**: `Prepare` fields, then `Finalize` (state root, transactions root, receipts root, bloom, gas used from execution, the empty uncle hash), then the seal.

**Sign transaction** (`contracts.CreateTransactionSign`). For each eligible block: to 0x89, value 0, **gas price 0**, gas 200,000, data `0xe341eaa4 ++ be32(number) ++ hash`, nonce `max(pool nonce, head-state nonce)`, EIP-155 legacy, signed by the validator key through the safety unit (`Sealer::sign_tx`: the key signs it, the record does not change). Eligible: `number % 15 == 0` (or before TIP2019), a block at most 30 behind the head (the contract refuses a block older than two epochs, and a refused transaction at the lowest nonce jams the sender), and a node that is in the candidate pool of the epoch, **penalised nodes included** (it is how a penalised masternode returns). The node signs its own blocks and the blocks it imports.

## Source

- `miner/worker.go:557-723` `commitTransactions` (`:579` gas, `:584` soft cap, `:619` skip account, `:661-690` sign filters, `:123-128` cap), `:725-806` `fillTransactions` (`:762-773` specials first), `:808-834` `splitSpecialTxs`; `core/txpool/txorder/ordering.go` (`:42-62` tip, `:75-86` order, `:92-` heap).
- `miner/xdpos_worker.go:940-1010` `commitMinedBlock`, `:1043-1100` `broadcastSigningTx`, `:1119-1172` `maybeBroadcastImportedSignTx`, `:1184` `isSignBlock`, `:1207` `isSigningTxStale`; `contracts/utils.go:95-100` `signTxNonce`, `:123-231` `CreateTransactionSign`, `:233-242` `CreateTxSign`; `consensus/XDPoS/xdpos.go:1658-1722` `FinalizeAndAssemble`, `engine_v2/engine.go:769-806` `Finalize`.

## Rust

`xone_builder::producer::{build::{select, assemble, Params, Commit, Roots}, signtx::{build, calldata, digest, encode_signed, emit, is_sign_block, is_stale, nonce}}`; execution: `xone_executor::execute_unsealed` (everything `execute_block` does after the header checks, for a header without roots and seal; the sealed block is imported through `execute_block` in full).

## Vectors and mutants

`tests/producer_construction.rs`: 200 V1 and 200 V2 Apothem blocks are rebuilt from their own transactions, timestamp, round and QC, and equal the canonical header hash (the roots of the recorded header stand for execution; the signature is the real masternode's after the test checks that our digest is what it signed). `tests/producer_vectors.rs`: the real sign transactions of the archive (nonce, gas 200,000, price 0, data) equal ours, and our EIP-155 digest recovers their senders; the pinned `CreateTxSign`, `isSignBlock`, `isSigningTxStale` (`oracle/run-producer.sh`). `tests/producer_select.rs`. Mutants: special transactions not first, price above zero, gas 100,000, no 15-block cadence.

## Not covered

The gas a transaction uses is its declared gas during selection (a block may be fuller than geth's, never over the limit); a transaction that execution refuses is dropped and the block is executed again. V1 checkpoint `Validators` (M2 tokens) are built only for one masternode (Go's `math/rand` source is not ported); the V1 penalty list before TIPSigning (0x89 state) and the comeback part (XB-10) are not built.
