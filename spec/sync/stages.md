# Staged sync: headers, bodies, senders, execution, finish

## Rule

The pipeline imports blocks in five stages. Each stage has one durable checkpoint, the last block it has finished, kept in the store. The checkpoint changes in the same transaction as the rows the stage wrote, so a stage is either done for a batch or not started for it (ADR-005 S5 and S6: kill points K14 before the commit, K16 after).

| Stage | Reads | Writes (one transaction per batch) | Bound |
|---|---|---|---|
| headers | the source | headers `ck+1..`, checkpoint `headers` | ADR-007 Q8: at most 2,048 staged headers above the head |
| bodies | staged headers, the source | bodies of the batch, checkpoint `bodies` | Q9: 128 blocks per batch, 64 MiB of transactions (the source stops at the byte budget, so no body is fetched twice) |
| senders | staged bodies | checkpoint `senders` (no rows) | one body batch at a time, `Limits::workers` threads |
| execution | staged header and body | the block commit of `Chain::insert` (ADR-005 S1-S7); its checkpoint is the head | Q10: 64 blocks per batch; Q11 holds because `insert` commits one block before it returns |
| finish | the head | checkpoint `finish`; staged rows up to the head are deleted | one transaction |

1. **Start at the checkpoint.** A stage starts at `max(its checkpoint, head) + 1` and stops at the checkpoint of the stage before it. After a crash the next run repeats at most the batch that was in flight. Nothing starts at block 0 on a store that holds a head.
2. **Backpressure is structural.** The pipeline is pull-driven: headers are not requested while 2,048 are staged above the head, and a stage never runs past its predecessor. The producer waits by not being called. Headers are never all in memory: a batch is validated, written and dropped (xOneGo plan A3, the 1.9M-of-85M header failure).
3. **Headers.** Consecutive numbers, each `parent_hash` equal to the hash of the previous header, the first equal to the head (or the last staged header). A break is `Link { number }`: the source's answer is wrong, or the rows staged before a restart are of another chain, and `unwind` restarts. The consensus check of each header runs once, in `Chain::insert`, before execution.
4. **Bodies.** Each body is checked against its header with `BodyVerifier::verify_body` before it is stored (geth `verifyBodyAgainstHeader`, `eth/sync_xdc.go:6444`: transactions root, uncle hash). The source returns a prefix when it cannot serve all; no body at all is `Stall { stage: Bodies, number }`, a typed error, never a wait.
5. **Senders.** Every transaction is decoded and its sender recovered, in parallel batches. A transaction that does not decode or recover stops the pipeline before execution, naming block and index. The executor recovers again: `BlockExecutor` takes the raw body.
6. **Execution is `Chain::insert`.** Header check, body check, execution on the parent's own state, receipts, bloom, gas, access list hash and state root, then the commit (`spec/chain/insert.md`). The pipeline has no other way to advance the head, no flag, and no "skip" or "trusted" path (xOneGo `243b2112`: never fall back to full execution on an anchored chain; here there is nothing to fall back from). An outcome other than `Extended` is `Unexpected`.
7. **Finish** moves the `finish` checkpoint to the head and deletes the consumed rows, so the store holds at most the Q8 window of staged rows.
8. **Unwind (reorg, set-head).** `stages::unwind(chain, to)` first deletes the staged rows above `to` and cuts every stage checkpoint to `to`, then `Chain::set_head(to)` unwinds the head (U1-U5). A crash between the two leaves the stages cut and the head where it was; the next run repeats. `Store::unwind_to` also cuts the `finish` checkpoint, which never stands above the head.
9. **The executor context.** `XdcExecutor` (real `execute_block`, V1) keeps what a header cannot give: deployed code, sign transactions with their receipt status, the V1 snapshot after the gap reseed, accounts above 2^256. Each block's share travels in `Execution::aux` and is stored in the block's commit (`Record::aux`). `XdcExecutor::restore` rebuilds the context from those rows after a restart or an unwind. A block joins the context only when its child arrives, so a block that failed to commit is never part of it. Above the V2 switch block it returns a typed error: the V2 header context belongs to the follower (XB-11).

## Source

- geth fetch shapes: `eth/downloader/downloader.go:45-47` (`MaxBlockFetch` 128, `MaxHeaderFetch` 192, `MaxReceiptFetch` 256); `eth/sync_xdc.go:115` (`xdpos2MaxHeaderFetch` 192: legacy peers answer a longer request empty), `:5298-5330` `fetchHeaderBatch`, `:5608` `fetchBodiesBatch`, `:6444-6457` `verifyBodyAgainstHeader`.
- geth import batching: `core/blockchain.go:2471-2508` `InsertChain` (contiguity check, one chain lock, `insertChain` at `:2510`), `:2551-2569` known blocks skipped.
- ADR-007 Q8 to Q11 (`docs/adr/ADR-007-operations-limits.md` section 2); ADR-005 sections 5 and 8 (commit and unwind steps), section 7 (reth comparison: the stage checkpoint against the static file tip, `storage/provider/src/providers/static_file/manager.rs:1735-1860`).
- xOneGo lessons: plan A3 (all headers in memory), `3a0782af` and `bce8733b` (redundant body fetch rounds), `243b2112` (no full-execution fallback on an anchored chain).

## Networks

All. The executor runs V1 blocks (Apothem to its switch block, mainnet likewise); V2 execution needs the follower context.

## Rust

`xone_sync::stages::{headers, bodies, senders, execute, finish, sync, unwind, Source, Limits}`, `Store::{checkpoint, staged_header, staged_body, stage_headers, stage_bodies, set_checkpoint, finish, unwind_stages}`, `xone_sync::xdc::XdcExecutor`.

## Vectors

`crates/xone-sync/tests/import.rs`: Apothem 0..=1,800 through the pipeline with the real executor (19 archive hashes equal, head state root equal, every checkpoint 1,800); a kill at each of ten points (staged transaction before and after commit in headers, bodies, senders and finish; execution at K05, K13, K16) restarts at the stage checkpoint, re-executes nothing that was committed and reaches the clean run's head and state root; the Q8 bound (`staged_headers_never_exceed_the_q8_window`); every body returned once in batches of at most 128 (`every_body_is_fetched_once_in_bounded_batches`); unwind then import again; a body that does not match its header and a file with a missing block stop before execution.

## Not covered

- The senders stage does not feed the executor (a `BlockExecutor` change).
- `MemStore` is the only store: staged rows and checkpoints are modelled with the ADR-005 transaction and kill points, the MDBX tables are the storage work item. The executor's code table, today inside `Record::aux`, becomes a table there.
- A snapshot with votes in flight cannot be stored (`XdcExecutor` refuses it; Apothem V1 has none).
- Receipts are not fetched: a full sync executes the blocks. Receipt batches (Q9) belong to fast and snap sync (XB-17).
- Header consensus verification for V2 blocks, and the 10M-block RSS gate.
