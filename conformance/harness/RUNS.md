# xone-replay runs

Sequential replay on xOne's own state (`xone-replay run`): genesis built from the chainspec alloc, each block executed on the persisted state of its parent (`StateProvider::commit_block`), compared with the canonical header on hash (every 100th block and the last, from the archive's own `eth_getBlockByNumber`), gasUsed, receiptsRoot, logsBloom, transactionsRoot, stateRoot, and, at V1 checkpoints, the masternode list in extraData derived from the snapshot. The first difference stops the run and writes `conformance/divergences/<network>-<block>.md`.

Oracle: Apothem archive on xdc02 (read only, 4 requests per second, 5 calls per request); geth-XDC pin `6fc8e8c2088b7f18264f5663036669d04fa7becf` (`conformance/PIN`). Blocks come from `debug_getRawBlock`, cached by `xone-replay fetch`; the replay itself is offline. Host: the build host of the 2026-11-11 session, release build, one thread. Commit: base `bd1b9ca4475cc76f2fbef8fba797abefdd642949` plus the uncommitted XA-05 tree.

| date | network | range | mismatches | blocks executed | wall time | note |
| --- | --- | --- | --- | --- | --- | --- |
| 2026-11-11 | apothem | 0..=16 | 1 at block 16 | 15 | 1 s | first run. Sign transaction to 0x89 (before `TIPSigning`) used 23,640 gas, archive 107,558. Cause: `Overlay::basic` returned the account without its code, so the EVM ran an empty contract. Fixed in `xone-executor` (`overlay.rs`, code is loaded with the account). No geth rule involved. |
| 2026-11-11 | apothem | 0..=2187 | execution error at block 2187 | 2186 | 2 s | account 0x38B2...34Be has a balance above 2^256 (geth-XDC `BigBalance`, `spec/evm/wide-balance.md`). The executor refused wide balances. Fixed: the journal side map is seeded from `Env::wide_balances` and kept for the whole block (`xone-executor/src/lib.rs`), the runner tracks wide accounts from the committed diffs. |
| 2026-11-11 | apothem | 0..=1800 | 0 | 1,800 | 3.8 s | gate. Includes the first V1 checkpoint reward at block 1,800 (blocks 1..900 rewarded) and 229 blocks with sign transactions run in the EVM. |
| 2026-11-11 | apothem | 0..=10000 | 0 | 10,000 | 105 s | 100 archive hash checks, 11 checkpoint lists derived |
| 2026-11-11 | apothem | 0..=20000 | 0 | 20,000 | 244 s | 200 archive hash checks, 22 checkpoint lists derived (every one equal), 0 checkpoints with penalties skipped. `--resume` from the checkpoint at 20,000: 0 blocks, 5 s. A resume from 1,000 continued through the block-1,800 reward at 0 mismatches. |

Block cache: `conformance/harness/cache/apothem.jsonl` (28 MB for 0..=20000, gitignored). Committed vectors: `conformance/vectors/replay/apothem-0-1800.jsonl.gz` (390 KB), read by `tests/replay.rs`.

Wall time grows with the state: the state root is recomputed from the flat tables for every block (`xone_state::trie`, no incremental trie yet).
