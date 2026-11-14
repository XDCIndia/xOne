# Offline import and export of block files

## Rule

The block file is the geth `export` format: the RLP of each block, one after the other, ascending, with no framing or index: `[header, [transactions], [uncles]]` (geth's `exportBlock` adds `Withdrawals` and a block access list as optional trailing fields; an XDC block has neither, and a file that has them is refused so that nothing is dropped silently).

1. **Import** feeds the staged pipeline (`stages.md`) through `FileSource`: headers are read forward in windows, their bodies are read again by offset, so no body is held while headers are staged. The blocks run through the real executor and `Chain::insert`, the same path as live import. There is no switch that skips a check.
2. **Skips.** Block 0 is skipped (geth `if b.NumberU64() == 0`), and blocks the chain already holds are skipped (geth `missingBlocks`): a file read again, or a file that starts below the head, continues at `head+1`. A file that starts above `head+1` is refused (`the file starts at block N, the chain needs block M`), and a truncated or undecodable block is an error, never a partial import that looks complete.
3. **After a restart** the source scans the file by item lengths to the block it needs (it does not decode). Stage checkpoints decide where the pipeline continues (`stages.md`).
4. **Export** writes blocks `first..=last` of the canonical chain, ascending, and fails when a block is missing or does not link to the one before (geth "chain reorg during export"). Genesis is block 0 and is written when asked for. `export` then `import` of the output is byte equal to the input: the typed transaction is an RLP string around the consensus encoding and the legacy one the RLP list, which is exactly how the block was read.
5. **CLI.** `xone import <file> [--network n]` prints blocks, rate, head and peak RSS. `xone export <file> [from] [to] --input <blockfile>` writes the range (the whole chain without `from` and `to`). The datadir has no block database yet (the MDBX binding of ADR-005), so both run on an in-memory chain from the network's genesis, and `export` takes its chain from `--input`. They move to the datadir with the store.
6. gzip archives, as geth (`:213-222`, `:479-499`): import detects gzip by its magic bytes (a `.gz` name without it is refused) and streams the archive once through a CRC-checking multi-member decoder into a spool file in `TMPDIR`, removed afterwards and on failure; the import reads the spool as a plain file, so a truncated or corrupt archive fails before one block is imported. Export writes gzip when the file name ends in `.gz` and reports success only after the gzip trailer is written and the file synced. An era-style chunked variant is not implemented.

## Source

- `cmd/geth/chaincmd.go:365` `importChain`, `:445` `exportChain` (`first last` arguments, `last` not above the head).
- `cmd/utils/cmd.go:58` `importBatchSize` 2,500 (here the batch is the staged window); `:188-267` `ImportChain` (gzip `:213-222`, RLP stream `:228`, skip block 0 `:245-249`); `:413` `missingBlocks`; `:433` `ExportChain`, `:458` `ExportAppendChain`, `:479-499` `exportChain`; `:61-91` `exportBlock` and `makeExportBlock`.
- `core/types/block.go:278-283` `extblock`, `:398` `EncodeRLP`; `core/blockchain.go:1840-1869` `ExportN` (the link check).
- `conformance/harness/src/cache.rs`: the cache line `{n, raw, hash?}` where `raw` is the archive's `debug_getRawBlock`, the block of an export file.

## Networks

All; the executor runs V1 blocks (`stages.md`).

## Rust

`xone_sync::import::{FileSource, decode_block}`, `xone_sync::export::{export, encode_block}`, `bin/xone/src/cmd/`.

## Vectors

`crates/xone-sync/tests/import.rs`: the committed Apothem vectors 0..=1,800 converted to a block file in the test; `export_then_import_is_byte_equal` (the exported 1,801 blocks equal the input byte for byte, and importing them reaches the same head); a range export; the errors for `first > last` and a block above the head; a file read twice, a file that starts at block 200, a file that continues a chain, a truncated file. `bin/xone/tests/cli.rs`: the two commands as processes.

## Not covered

- Round trip of 1,000,000 blocks (the vectors hold 1,801; the harness cache of 20,000 blocks is not present and is fetched from the archive, which was not needed for this slice).
- era1 or an era-style chunk layout; resuming after `kill -9` of the CLI process (the CLI store is in memory; the ten kill points run on `MemStore` in the tests).
