# Decision record: storage and storage architecture of the xOne clients

Issue [#135](https://github.com/XDCIndia/xOneRS/issues/135). Status: **proposed**; the owner decides (section 7). This record compares the stores the four xOne clients use today (Rust, C++, TypeScript, Python), with the sizes and latencies that are measured in the tracker comments on issue #140 and in the TB-17 archive pull requests (xOneC#80, xOneTS#89, xOnePy#95). It does not run a benchmark; the benchmark plan of the issue (trace recorder, baselines B0, stage 1 and 2) is not started.

Pins. xOne `origin/main` `a99fcf7290fc9a78be30a700badb7504a931be2a`. Sources read on 2026-11-14: `docs/adr/ADR-005-storage-and-publication.md`, `docs/ops/{net2040-follow,apothem-follow,net2040-sync-modes}.md`, `crates/xone-db/src/mdbx.rs`, `Cargo.lock`; xOneC `docs/adr/ADR-005-cpp-addendum.md`, `src/db/store.hpp`; xOneTS `docs/adr/ADR-005-ts-addendum.md`, `docs/ops/net2040-ts-sync-modes.md`; xOnePy `docs/adr/ADR-005-py-addendum.md`, `docs/ops/net2040-py-sync-modes.md`; PRs xOneC#80, xOneTS#89, xOnePy#95; issue #140 (29 hourly comments, last 2026-11-14 05:30Z).

## 1. Question

Which database each client uses for which node role, what is known about its cost, and what must be measured before an engine is changed. The issue proposes one answer for all clients: **two layers**, a small mutable hot-state store and append-only segment files for history, and **no engine change**. This record checks that proposal against the code and the measurements that exist now, and corrects its client count (the issue names three clients; four xOne clients exist and all four use the two-layer layout).

## 2. What each client uses today (verified in code or docs)

| Client | Hot store | Engine pin | Durability | History layer | Evidence |
| --- | --- | --- | --- | --- | --- |
| xone-rs (Rust) | MDBX env `chain.mdbx`: state, changesets, canonical index, segment index, consensus tables, `Meta` | crate `libmdbx` 0.6.6, `mdbx-sys` 13.11.0 (`Cargo.lock`) | producers `SyncMode::Durable`, WRITEMAP off; followers `SafeNoSync` group commit (`crates/xone-db/src/mdbx.rs:5`, `:150-190`) | `XSEG` CRC-32C segment files `static/{headers,bodies,receipts}/`, valid only up to the committed manifest | ADR-005 section 1 |
| xOneC (C++) | libmdbx env, 17 tables including `AccountHistory` and `StorageHistory` (archive only) | libmdbx v0.13.12 C API at a pinned commit, no patch | `MDBX_SYNC_DURABLE`, WRITEMAP off | the same `XSEG` frames and manifest | xOneC `src/db/store.hpp`, ADR-005 C++ addendum |
| xOneTS (TypeScript, Bun) | `bun:sqlite` WAL, `synchronous=FULL`, one `WITHOUT ROWID` table per MDBX table | the Bun binary's built-in SQLite | a returned commit is on the device (one WAL fsync) | `XSEG` segments | ADR-005 TS addendum |
| xOnePy (Python) | stdlib `sqlite3` WAL, `synchronous=FULL`, explicit `BEGIN` and `COMMIT` | the interpreter's SQLite | as TS | `XSEG` segments | ADR-005 Python addendum |
| RocksDB | used by **no** xOne client. Measured once as a C++ alternative (below) and named by the issue as the Rust fallback | n/a | n/a | n/a | xOneC ADR-005 addendum "Spike result" |
| xOneGo (Go, for reference) | Pebble v1.1.5 with empty options; `FileDB` is still the default chaindata backend | per the issue | n/a | own freezer | issue #135 "Why" item 1 (not re-read here) |

Common to the four: the commit order of ADR-005 section 5 (journal, execute, stage segments, fsync segments, fsync directories, one hot-store transaction that writes the segment index, canonical index, generation and manifest, commit, publish), the kill points K01 to K19, the frame and manifest formats. Only the engine under the hot store differs.

## 3. What is measured

### 3.1 Durable commit latency (spike, one commit per block)

Workload of all four spikes: 2,000 simulated blocks of 56.6 KB (header, body, receipts, 200 state rows), one durable commit per block, ext4 on NVMe, shared host. The hosts, loads and runtimes differ between rows, so **the rows are not comparable as a ranking** (`docs/perf.md` rule 4); they show that every engine costs about one device flush (about 20 ms).

| Engine and mode | Client spike | p50 ms | p95 ms | p99 ms | max ms | Sync calls per block |
| --- | --- | --- | --- | --- | --- | --- |
| libmdbx, durable commit alone | C++ | 22.7 | | 55.8 | | 1.00 (`fdatasync`) |
| RocksDB, `WriteOptions::sync` | C++ | 21.7 | | 65.3 | | 1.01 (`fdatasync`) |
| libmdbx, full ADR-005 order | C++ | 195.5 | | 342.7 | | 10.00 |
| RocksDB, full ADR-005 order | C++ | 163.8 | | 365.1 | | 10.01 |
| sqlite WAL `synchronous=FULL` | TS (Bun), load about 18 | 20.73 | 40.23 | 54.44 | 127.79 | 1.08 |
| sqlite WAL `synchronous=NORMAL` | TS | 0.34 | 3.48 | 41.41 | 105.07 | 0.07 (not durable after power loss) |
| lmdb-js 3.5.9, sync on commit | TS | 21.56 | 29.54 | 36.48 | 64.14 | 1.00 |
| sqlite3 WAL `synchronous=FULL` | Python, load about 40 | 22.40 | 45.93 | 71.32 | 149.91 | 1.08 |
| lmdb 3.0.0, `sync=True` | Python | 27.25 | 64.65 | 141.89 | 699.81 | 1.00 plus meta page |
| segment append and fsync only | TS / Python | 19.99 / 20.82 | 23.71 / 40.21 | 28.06 / 69.43 | 72.46 / 162.74 | 1.00 |

The Rust follower's fsync cost is recorded as 10 to 20 ms in the issue; no Rust spike table with this workload was found in the files read.

Other measured facts from the C++ spike (xOneC ADR-005 addendum): kill harness (SIGKILL at K01-K14, K16-K19 and at random times, torn segment tails) 114 rounds libmdbx and 76 rounds RocksDB, **0 failures each**; library build 8 s and 29 MB for libmdbx against 276 s (about 1,975 CPU s) and 391 MB for RocksDB; file size after 2,000 blocks 201 MB libmdbx against 116 MB RocksDB (stated as given up). The Python addendum records 873 crash-disk images (F1-F5) and 10 SIGKILL tests each for sqlite3 and lmdb, 0 failures.

### 3.2 Sizes on disk and memory

Each row is copied from the file named; none was re-measured. The tracker comments on issue #140 were all read (29 comments): they carry heads, lags, board status and mint rows and **no sizes**, so sizes come from the TB-17 PRs and the ops documents.

| Client | Node and state of the chain | Datadir | Parts | RSS | Source |
| --- | --- | --- | --- | --- | --- |
| Rust | net2040 full node at 762,846 | 17 GB (20 GB with segments) | not split | peak 7.07 GB (before the memory change) | `docs/ops/net2040-sync-modes.md` |
| Rust | Apothem full node at 766,981 | 20 GB | `chain.mdbx/mdbx.dat` 2.9 GB, 2.8 GB of it resident | 4.21 GB (RssAnon 1.41 GB, RssFile 2.80 GB) | `docs/ops/apothem-follow.md` |
| Rust | Apothem at 1,066,000 | 1.7 GB | not split | 1.1 to 1.2 GB (RssAnon 0.5 to 0.6 GB) | `docs/ops/apothem-follow.md` |
| Rust | net2040 minimal (snap) | 72 MB | not split | 78 MB after the install | `docs/ops/net2040-sync-modes.md` |
| Rust | net2040 archive (`xone-rs-net2040-archive-h4`) | not recorded; state is retained from block 825,238 only (xOneRS#152, "no true state archive" in the tracker) | | | tracker row, issue #140 |
| C++ | net2040 archive at 820,698 (`xone-c-net2040-archive-h95`) | 20.7 GiB | libmdbx 4.4 GiB, segments 16 GiB | about 6.2 GiB | xOneC#80 |
| C++ | same node at block 100,000 | 15.9 GiB | most of it from the 0..46k load-test range | | xOneC#80 |
| TS | net2040 snap, fast, minimal | 15.5 to 18.4 MB (30.2 MB before a wipe) | not split | peak 560 MB (snap), 335 to 350 MB after | `docs/ops/net2040-ts-sync-modes.md` |
| TS | net2040 archive at 20,902 (dense load-test range) | 2.26 GB | not split | 2.0 to 2.8 GB | xOneTS#89 |
| Python | net2040 snap, minimal | 20.7 to 21.8 MB | SQLite rows plus the segments of the window | peak 104 MB (snap) | `docs/ops/net2040-py-sync-modes.md` |
| Python | net2040 archive at 46,458 (end of the load-test range) | 14 GB | not split | 400 to 490 MB | xOnePy#95 |

Arithmetic on the split rows (mine, not measured): the hot store is 4.4 / 20.7 = 21 percent of the C++ archive datadir and 2.9 / 20 = 14.5 percent of the Rust Apothem datadir; the segments are 16 / 20.7 = 77 percent of the C++ archive. This supports the issue's claim that history is the bulk of the bytes. **Caution:** the C++ and Python archives and the Rust net2040 figure include the dense load-test range (blocks about 21,970 to 46,458 carry up to 20,000 transactions each; xOneC#80, xOnePy#95), so a size per block cannot be extrapolated to Apothem or mainnet from these rows.

Tracker state at the last comment read (2026-11-14 05:30Z, net2040 tip 830,494): Rust, Python and C++ archive nodes at the tip (C++ `245/245` and Python `124/124` historical reads equal the geth archive); the TS archive node was at block 89,333 (syncing from genesis, 130/130 equal below its window; ETA about 08:45Z). Sizes at the tip for the Python and TS archives are therefore not available yet.

## 4. The three layouts compared

"A" is MDBX plus static files (Rust, C++). "B" is SQLite plus static files (TS, Python). "C" is a KV engine alone, as RocksDB would be in the issue's section 2 matrix (nobody runs it). The 14-case score table of the issue stays the reference for the engines it covers; this table keeps only what the four clients' files and spikes support.

| Property | A: libmdbx plus segments | B: SQLite plus segments | C: RocksDB (not run by any client) | Evidence |
| --- | --- | --- | --- | --- |
| Durable commit cost | one `fdatasync`, 22.7 ms p50 in the C++ spike | one WAL fsync, 20.7 to 22.4 ms p50 in the TS and Python spikes | one `fdatasync`, 21.7 ms p50 in the C++ spike | section 3.1 |
| Crash safety under the ADR-005 kill points | 114 rounds, 0 failures (C++) | 10 SIGKILL tests and 873 crash-disk images, 0 failures (Python) | 76 rounds, 0 failures (C++) | section 3.1 |
| Native dependency | C library (one amalgamated C file, 29 MB build tree) | none: built into Bun and into CPython | large C++ build, 391 MB tree, 276 s | xOneC and xOneTS addenda |
| Hot store share of the datadir | 14.5 to 21 percent (rows above) | not split | not measured | section 3.2 |
| Reclaim after pruning | the file does not shrink (issue, measured in operations) ; segments are deleted whole | `VACUUM INTO` or a compacting copy (issue [W]) | compaction (issue [W]) | issue #135 "Why" and section 3 |
| Online backup | `mdbx_env_copy`; a raw copy of a live file gave a corrupt copy (issue, measured) | backup API or `VACUUM INTO` (issue [W]) | checkpoint by hard links (issue [W]) | issue #135 |
| Start-up and memory at tip | Rust: before the bounded-history change every header and transaction hash was in memory (7 GB at 762,000 blocks, `docs/ops/apothem-follow.md:8-10`); after it RssAnon 1.41 GB at the head and 0.22 GB RSS after a restart at the head (`:62-65`) | TS: about 15 minutes on a 3.2 GB datadir because `loadState()` reads every state row (issue, `packages/db/src/store.ts:1481-1512`) | n/a | issue #135; apothem-follow |
| Platform reach | Linux; macOS and Windows need native builds | inside the Bun binary and CPython: one artifact per platform | hard on small platforms | issue #135 case 11 |
| Supply chain | single maintainer, hosting moved; signed tag identical on GitFlic and GitHub and byte-equal to reth's vendored copy (xOneC addendum) | the interpreter's SQLite | upstream tag | xOneC addendum; issue #135 case 14 |

Reading: the measured differences between engines are small next to the layout. Every engine costs one device flush per durable commit; every engine passed the kill harness it was given; the costs that matter in operations (start-up scans, memory, file that does not shrink, unsafe copies) come from what is kept in the hot store and in process memory, not from the engine. That is the issue's thesis; the files read here do not contradict it and do not prove it either (no like-for-like benchmark exists).

## 5. Proposed decision (for the owner)

1. **Adopt the two-layer rule for all four clients**: a record that does not change after its block is final does not live in the hot store; it goes to a segment. Today the rule is broken by changesets and the transaction index in the hot store (Rust, C++, TS, Python all keep changesets in the hot store) and by in-memory indices (issue #135 "Why").
2. **Keep every engine**: Rust libmdbx 0.6.6, C++ libmdbx v0.13.12, TS `bun:sqlite`, Python stdlib `sqlite3` (Go Pebble per the issue). RocksDB is the named fallback for Rust and C++, not adopted.
3. **Change an engine only by the switch rule of the issue**: a challenger passes every hard gate and wins by 25 percent or more on two primary measures and loses none by more than 10 percent, on the same host, trace and durability mode.
4. **Do the layout work before any engine work**, in the order of the issue (hashed-key format bump first; then changesets and the transaction index to segments; persisted index so a restart does not read bodies; on-disk state reads in TS; backup and reclaim commands).
5. **Record every result with host, commit, flags and durability mode** (`docs/perf.md`).

## 6. What is not known (evidence gaps)

- No like-for-like benchmark exists. The four spike tables use different hosts, loads and runtimes; they measure one commit per block only, not throughput, reads, restart time or compaction.
- There is no RocksDB, fjall or redb measurement in Rust, and no libmdbx measurement in Bun.
- Sizes: no split of hot store against segments for the TS, Python or Rust-net2040 rows; the Rust archive size is not recorded; the TS and Python archives were not at the tip when their PRs were written, and the load-test range inflates every archive figure.
- The issue's gates G1 to G12 have no baseline B0 yet (the issue says so).
- The libmdbx file does not shrink, the raw-copy corruption and the TS 15-minute start-up are quoted from the issue as measured in operations; this record did not re-read those operation records.

## 7. Owner decisions (one line each)

1. Accept the two-layer rule for all four clients (section 5, item 1).
2. Accept "keep every engine; RocksDB is a fallback only" (item 2), or order a Rust RocksDB measurement first.
3. Accept the switch rule of the issue (item 3).
4. Name the bench host for stage 1 and 2 (a host with no seated validator, load below 2, 30 percent free disk, per the issue's host rule).
5. Decide whether the load-test range (blocks about 21,970 to 46,458) stays in future size measurements or is excluded, so sizes can be compared with Apothem and mainnet.

## 8. Sources

Issue #135 (all sections); issue #140 and its 29 comments; PR xOneC#80 ("Proof on net2040"), xOneTS#89, xOnePy#95; the files listed under Pins. Nothing was re-measured and no live node was touched.
