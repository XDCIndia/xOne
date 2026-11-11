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

## XA-05 slice 2

### Incremental state root

`xone_state::mpt` replaces the whole-table recomputation: a node-cached Merkle-Patricia trie over the `alloy_trie` node encoders (`LeafNodeRef`, `ExtensionNodeRef`, `BranchNodeRef`), one storage trie per account, updated from a block's diff; only the nodes on the paths a diff touches are encoded again. `trie::root_of_hashed` and `flat_root` stay as the reference. `StateView::root_with` applies the diff to the head's trie and keeps it pending, so the commit of the same diff costs nothing; an unwind applies the changesets the same way. `crates/xone-state/tests/incremental.rs`: the incremental root equals the reference root after each of 10,000 random diffs (accounts created, changed and deleted with their storage, slots set and cleared, wide balances), before each commit, with a diff that is asked for and not committed, and after random unwinds of up to 150 blocks (a mutant that skips the single-child collapse of a branch fails at block 5).

Replay 0..=20,000 of Apothem, same binary and flags as the first row, back to back on the same busy host (load average about 25):

| trie | wall | user | note |
| --- | --- | --- | --- |
| whole table (before) | 402.9 s | 328.2 s | 233.6 s on the quieter host of the original run |
| incremental (after) | 82.1 s | 51.7 s | 23.2 s on the quieter host |

Per-block cost no longer grows with the state: 0..=20,000 in 23.2 s and 0..=50,000 in 57.4 s on the quiet host (1.16 and 1.15 ms per block), 0..=432,000 below.

### Gate runs, XA-05 slice 2

Host: the shared build host (load average 20 to 25, other tenants), release build, one thread; wall times include that load. Oracle and pin as above. Base commit `5e99b97abc72677e658a0f01b0ec3e84c3d8fa3e` plus the XA-05 slice 2 tree.

| date | network | range | mismatches | blocks executed | wall time | note |
| --- | --- | --- | --- | --- | --- | --- |
| 2026-11-11 | apothem | 0..=256,000 | 0 | 256,000 | 631.8 s | genesis start, 2,560 archive hash checks, 284 checkpoint lists derived |
| 2026-11-11 | apothem | 0..=432,000 | 0 | 432,000 | 894.3 s | genesis start, 4,320 archive hash checks, 480 checkpoint lists derived. The last 100,000 blocks took about 110 s. Not reached: `TIPSigning` 3,000,000. The limit is the fetch of the block cache (about 125 blocks per second at at most 4 requests per second), not the trie: 3,000,000 blocks need about 6.7 hours of fetching and about 1 hour of replay. |
| 2026-11-11 | apothem | DIAGNOSTIC-PIVOT 56,826,900, then 56,826,901..=57,099,999 | 0 | 273,099 | 2,225 s (8.0 ms per block steady state on the loaded host, 4.8 s for the first 1,000) | two epochs before the V2 switch block 56,828,700 and 302 V2 epoch switches after it. The state of 56,826,900 is the archive's (234,877 accounts, 6,367,277 slots, wide balance of 0xa650...c5 read from its leaf), rebuilt root equal to the header's `stateRoot`. Every switch: `Validators` and `Penalties` derived by the runner (first V2 block from the V1 list of the switch block, later ones from the gap snapshot built from the runner's own 0x88 state, penalties from the V2 engine) and equal to the header's; 301 gap snapshots; V1 checkpoint rewards at 56,827,800 and the V2 switch block, V2 flat rewards at every switch, TRC21 token fees in every block; 3-chain commit: 270,678 blocks committed, highest 57,099,996 (round 271,743). 1,000 switches (900,000 blocks) not reached: the block cache stops at 57,100,000 (fetch rate), the replay itself would take about 2 hours at this load. |
| 2026-11-11 | apothem | DIAGNOSTIC-PIVOT 83,596,000, then 83,596,001..=83,602,999 | 0 | 6,999 | 51 s | across `TIPFixedReward` / `TIPUpgradePenalty` / `Gas2500x` 83,600,000: 9 V2 epoch switches, 5 before (83,596,354 .. 83,599,875) and 4 from 83,600,502 on, the first of which pays the first tiered reward (masternodes, protectors, observers, MintedRecord 0x9a bookkeeping, burnt fee). `Validators` and `Penalties` derived and equal at every switch (post-upgrade penalty rule, 5 epochs of history). State 325,043 accounts, 8,579,440 slots, 73 accounts held as a storage root only (see below). The gap snapshots of the four gap blocks below the pivot come from the archive (`debug_storageRangeAt` on 0x88, built by `gap_snapshot::build`) because the first switch after the pivot reads the gap block 83,595,150. |
| 2026-11-11 | apothem | DIAGNOSTIC-PIVOT 84,729,000, then 84,729,001..=84,735,500 | 0 | 6,500 | 130.6 s | across `TIPProportionalReward` 84,733,000 (Prague since 84,333,000, so every block stores the parent hash in the EIP-2935 contract): 7 V2 epoch switches, 4 before (84,729,735 .. 84,732,420) and 3 from 84,733,320 on (the first proportional reward). Contracts reached by the window were downloaded in full (7 of them found by `callTracer` over the window, one with 1.76 million slots). |
| 2026-11-11 | apothem | mutant `tier-off` (tiered reward switched off), pivot 83,596,000 | diverges at 83,600,502 | 4,501 | | `stateRoot` differs at the first tiered reward switch, the expected block. |
| 2026-11-11 | apothem | mutant `gap-wrong` (previous epoch's gap snapshot), pivot 83,596,000 | none | 6,999 | | equivalent mutant on this window: the 47 candidates of the previous epoch are the same set, so `Validators` come out the same. Caught in the 56,828,700 window (below). |

Tests: `crates/xone-state/tests/incremental.rs` (10,000 random diffs and unwinds). `conformance/harness/tests/v2_replay.rs` (blocks 56,827,800..=56,830,294, 2.9 MB gzipped, `conformance/vectors/replay/apothem-v2-56827800-56830294.jsonl.gz`; `#[ignore]`, needs the 294 MB pivot state at 56,828,700 in `cache/`, which cannot be committed: 5.3 million slots, 143 MB even pruned to what the window touches, 359 MB of contract code): the first three V2 epoch switches replay with 0 mismatches (3 switches, 2 gap snapshots, V2 flat reward at 56,829,393); the mutant that reads the gap snapshot of the epoch before diverges at the third switch 56,830,293 (the first one that reads a gap snapshot; no snapshot exists for that gap in the V1 era); the mutant `reward = 0` diverges at the second switch 56,829,393 (`stateRoot`, the first V2 reward).

### DIAGNOSTIC-PIVOT notes

`xone-replay pivot --network apothem --block N --to M --source CACHE` loads the archive's post-state of block N and refuses it unless the rebuilt root equals the header's `stateRoot`, then replays N+1..=M on xOne's own state. The state is archive-sourced until XA-09 authenticates anchors: it proves execution and consensus rules, not bootstrap trust. Sources: `debug_accountRange` (with the preimage of every address), `eth_getCode`, `debug_storageRangeAt` on block N+1 at transaction 0 (with the preimage of every slot), and the account leaf from `eth_getProof` for the accounts that can be above 2^256 (the `balance` of `debug_accountRange` and of `eth_getProof` is modulo 2^256: 0xa650...c5 is wide at 56,826,900). Downloads are resumable (`<state>.stage1`, `<state>.storage`), batched (at most 4 requests per second). Download time of a full state at this size is 1.5 to 2 hours, which exceeds the budget; so an account with more than 100,000 slots is held as its storage root only (`Dump::opaque`, `StateProvider::with_opaque`): the root of the whole state is still checked against the header, and a block that reads or writes such an account no longer matches (that happened twice in the 84,729,000 window, at blocks 84,729,112 and 84,729,811, until the contracts the window calls were downloaded in full). The pivot is not a gate for bootstrap: 56,826,900 is the start two epochs before the V2 switch block (the budget did not force a later pivot).

Divergences found and fixed during this work, none a consensus-rule bug of a crate:

| block | what | cause and fix |
| --- | --- | --- |
| 56,826,900 | pivot state refused (rebuilt root differs) | the balance of 0xa650...c5 was read modulo 2^256; read from the account leaf now (`pivot::fix_wide`, geth-XDC `BigBalance`, `spec/evm/wide-balance.md`) |
| 56,828,700 | `extraData` list of the V1 switch checkpoint differs (15 derived, 13 in the header) | the runner's auxiliary check ignored the penalties of earlier checkpoints: `Prepare` removes the `Penalties` of the checkpoints 1 to 4 epochs back as well (`consensus/XDPoS/xdpos.go:1468-1472`); `checkpoint::checkpoint_penalties` is used now, which also removes the `aux_skipped` case |
| reward walk | the V2 reward window walks back over V1 checkpoints | geth's `XDPoS.IsEpochSwitch` is `number % epoch == 0` up to the V2 switch block (`consensus/XDPoS/xdpos_api.go:228-235`): the store marks V1 checkpoints as switches |
| 84,729,112, 84,729,811 | gas, receipts, bloom and state differ | pruned state: a called contract (directly, then through an internal call) was held as a storage root only |
