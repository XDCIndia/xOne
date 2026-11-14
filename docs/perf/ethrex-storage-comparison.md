# ethrex and xOneRS: storage, execution pipeline and speed

Status: analysis plus two measured changes. Every claim cites a file and line. ethrex is
`lambdaclass/ethrex` at `740ea49` (2026-11-14); paths below are relative to that checkout. xOneRS is
`XDCIndia/xOneRS` at `b055158` (measurements) and `1c30643` (this document). Numbers from this host are labelled with the host load; numbers from
ethrex are labelled with what ethrex itself states.

Improvements are ranked in section 4 (issues #243 to #250, label `perf`); section 5 has the two implemented (#319, #334). Measurement rules: `docs/perf.md` (sections 1 to 4).

## 1. Side by side

| Area | ethrex | xOneRS |
| --- | --- | --- |
| Backend | RocksDB for a real datadir, in-memory for tests; no libmdbx in the store (`crates/storage/store.rs:530-536`, `crates/storage/backend/mod.rs:3-7`, `cmd/ethrex/initializers.rs:279-287`) | libmdbx env `chain.mdbx` plus append-only segment files for headers, bodies and receipts (ADR-005 section 1; `crates/xone-db/src/mdbx.rs:150-161`, `crates/xone-db/src/segments.rs:26`) |
| Durability per commit | `db.write(batch)` with the WAL on and `use_fsync(false)` (fdatasync), WAL and SST synced every 32 MB (`crates/storage/backend/rocksdb.rs:118-121`, `:746-754`) | `SyncMode::Durable`, about 8 fsyncs per block; a non-signing node defers them for 1 s (`GROUP_COMMIT`) (ADR-005 section 4; `crates/xone-node/src/services/core.rs:67`, `crates/xone-sync/src/mdbx/mod.rs:980-1089`) |
| Chain data | `HEADERS`, `BODIES`, `RECEIPTS_V2` column families keyed by block hash, LZ4 (`crates/storage/api/tables.rs:16-45`, `crates/storage/backend/rocksdb.rs:158-165`) | Segment frames per block, CRC per frame, visibility by the manifest in the MDBX commit (ADR-005 sections 5, 6); no compression yet (#194) |
| Flat state | `ACCOUNT_FLATKEYVALUE`, `STORAGE_FLATKEYVALUE`, filled by a background generator; `Trie::get` reads it when the path is covered (`crates/common/trie/trie.rs:145-152`, `crates/storage/store.rs:2617-2635`) | `HashedAccounts`, `HashedStorages`, `Bytecodes` written in every block commit (ADR-005 section 1, schema 2); read through `MemFlat` (default) or `DiskFlat` with two-generation LRUs (`--state.disk`, `crates/xone-sync/src/mdbx/diskflat.rs:1-40`) |
| Trie nodes on disk | Keyed by nibble path, single version (`crates/storage/trie.rs:110-131`, `crates/storage/store.rs:721`, `:2749`) | Keyed by owner and nibble path in `TrieNodes`, with `--state.disk` only; else every node in memory (`crates/xone-state/src/mpt.rs:33-43`, #145) |
| In-memory layers | One diff layer per block, 128 kept, a global bloom filter in front of the layer walk, commit by depth (`crates/storage/layering.rs:84-140`, `:260-285`; `crates/storage/store.rs:72`) | One head trie; the diff of the block under test is applied and undone (`crates/xone-state/src/provider.rs:63-91`); changesets keep 4,096 blocks openable (ADR-005 section 16) |
| Write path | One `UpdateBatch` per block to a single persist worker over a bounded channel; the block pipeline does not wait for the flush; a `BlockDataBuffer` serves reads of unflushed blocks (`crates/storage/store.rs:2371-2425`, `:2641-2735`; `crates/blockchain/blockchain.rs:2573`; `crates/storage/block_data_buffer.rs:30`) | Journal file, segment frames, MDBX transaction and publish, in order, on the import thread for every block (ADR-005 section 5; `crates/xone-sync/src/mdbx/mod.rs:525-560`, `:980-1089`) |
| Reorg and history | `STATE_HISTORY` reverse diff per block, pruned at finality (`crates/storage/journal.rs:12-20`) | `AccountChangeSets` and `StorageChangeSets` hold whole prior records, 4,096 blocks (ADR-005 sections 1, 16) |
| Sender recovery | rayon `par_iter` over the block (`crates/common/types/block.rs:345-358`); libsecp256k1 by default, k256 only for zkVM guests (`crates/common/crypto/provider.rs:76-97`, `crates/common/crypto/Cargo.toml:44`) | Scoped threads, at most 8, chunks of at least 256 (`crates/xone-executor/src/lib.rs:701-730`); the node recovers ahead on a pool of one worker per core (`crates/xone-node/src/services/senders.rs:58`, #141); **k256** for every recovery (`crates/xone-primitives/src/seal.rs`, before this work) |
| Execution and merkleization | Four threads per block: warmer, trie prefetch, execution, merkleizer; execution streams account updates to a 17-thread merkleizer sharded by first nibble (`crates/blockchain/blockchain.rs:579-580`, `:1051-1250`) | Execution, then tx root, receipt root and state root one after the other on the same thread (`crates/xone-executor/src/lib.rs:482-486`); storage roots computed one account at a time (`crates/xone-state/src/mpt.rs:597-660`) |
| Warming and caches | Speculative parallel re-execution by sender group, a mempool prewarmer, a 2 GiB bytecode cache with stored jumpdest analysis (`crates/vm/backends/levm/mod.rs:3293`, `crates/blockchain/prewarm.rs:1-60`, `crates/storage/store.rs:344`) | No warmer. Bytecode analysed again every time an account is loaded (`crates/xone-executor/src/overlay.rs:248-257`) |
| Duplicate work | Receipts root and bloom validated once, in the execution thread | In the node path the tx root and the receipts root are each computed twice: by `run_block` and by `BodyVerifier` in `Chain::insert` (`crates/xone-executor/src/lib.rs:482-484`, `crates/xone-sync/src/chain/insert.rs:126`, `:152`) |
| Build | `lto = "thin"`, `codegen-units = 1`, jemalloc with a background purge thread by default (`Cargo.toml:54-57`, `cmd/ethrex/ethrex.rs:17-35`, `cmd/ethrex/Cargo.toml:84`) | The same thin LTO and one codegen unit, plus `strip` (`Cargo.toml` `[profile.release]`, from 7e7810a); system allocator |
| Snap | snap/1 with healing (deepest node first), snap/2 flat download with BAL patching (`crates/networking/p2p/sync/healing/storage.rs:124-163`, `crates/networking/p2p/sync/snap2/mod.rs:1-8`) | snap/1 plus xpre/1, re-pivot, heal, streamed into the tables with `--state.disk` (#143, #148) |

## 2. Measured numbers

### ethrex (as ethrex states them)

ethrex publishes no sync time, disk size or blocks per second in the README, roadmap or case studies
(`docs/case-studies.md:17`: "Case studies are coming soon"). What the tree contains:

- Synthetic ETH-transfer baseline, 100 blocks of about 404 transactions: 12.68 ms per block, about 669
  Mgas/s, no hardware stated (`docs/perf/README.md:127-131`; template values in `docs/perf/tracking.md:15-80`).
- Bytecode cache: Plataberget blocks 294,611..294,805 from 145.3 to 34.2 ms per block in replay; blocks
  that reuse 16k+ contracts from 30-44 ms to 19-21 ms (`CHANGELOG.md:43`, `:47`).
- Jumpdest analysis: 38-54 to 570 Mgas/s on random-bytecode blocks, Zen 2 (`CHANGELOG.md:53`).
- jemalloc purge inline cost "about 12% of total block execution CPU time", measured with perf on
  mainnet blocks (`cmd/ethrex/ethrex.rs:29-31`).
- Trie insertion 1.5-2x faster after a double-encode fix, Apple M3 Max (`docs/perf_reports/2025-04-03.md:5`).
- Parallel and pipelined merkleization (`CHANGELOG.md:112`, `:182`, `:344`): no number given.

### xOneRS (before this work)

- net2040 full sync, genesis to tip: about 95 min, 17 GB, peak RSS 7.07 GB (`docs/ops/net2040-sync-modes.md:39`).
- Dense ranges after #141: 15.6 to 25.5 blocks/s and 4,316 to 7,042 tx/s on 13,5xx..20,000; geth-XDC on
  the same data 22.2 blocks/s and 12,521 tx/s (PR #141 body, load1 48 to 75).
- Sequential replay 0..21,600: 387 s, 0 mismatches (PR #141 body).

### Profile of the replay on this host (2026-11-14)

`xone-replay run --network net2040 --to 20000`, sequential, genesis start, release builds with temporary
phase timers (not committed), `nice -n 19`. Host: shared build host, Intel i5-13500, 20 logical CPUs.
Both runs 0 mismatches.

| Phase (sum over 20,000 blocks, s) | `b055158` (k256), load1 42 | with #243 (libsecp256k1), load1 37 |
| --- | --- | --- |
| Sender recovery (`decode_all`) | 256.7 (71%) | 119.1 (56%) |
| Header and QC verification | 48.7 together with the body checks (14%) | 19.3 (9%) |
| Body and receipt checks (`BodyVerifier::verify`) | (in the line above) | 17.8 (8%) |
| Transaction loop (revm) | 24.1 (7%) | 23.7 (11%) |
| tx root, receipts root and bloom | 18.5 (5%) | 18.7 (9%) |
| State root | 11.3 (3%) | 13.8 (6%) |
| `execute_block` total | 359.4 | 214.5 |
| `StateProvider::commit_block` (outside `execute_block`) | 3.6 | 3.8 |
| Run, wall | 371.4 | 224.1 |
| Process CPU, user + sys | 872.8 | 404.9 |

The dense blocks of net2040 are value transfers to new addresses with empty data (block 13,650: 1,541
transactions; 44,000: 4,761). Signature recovery dominates; the EVM does not. Recovery microbenchmark on
this host, 20,000 signatures, CPU microseconds per recovery: xOneRS k256 path 99 to 182, k256
`recover_from_prehash` 240 to 373, libsecp256k1 52 to 77 (three rounds, load 36 to 87).

## 3. What makes ethrex fast, and what applies to xOneRS

1. Libsecp256k1 for recovery, on every core (`crates/common/crypto/provider.rs:76-97`,
   `crates/common/types/block.rs:345-358`). Applies directly: recovery is 71% of replay time here.
2. Thin LTO and one codegen unit (`Cargo.toml:54-57`). Already in xOneRS (`[profile.release]`, 7e7810a).
3. Execution, prefetch and merkleization pipelined on separate threads, merkleization sharded 16 ways
   (`crates/blockchain/blockchain.rs:1051-1250`, `:579-580`). Applies to the roots: 16% of
   `execute_block` here (tx, receipts and state roots); more on blocks that write much storage.
4. Disk off the critical path: the persist worker acks after staging (`crates/storage/store.rs:2641-2735`).
   Applies in part: ADR-005 publishes a head only after its MDBX commit; the next block can still
   execute while the previous one commits, as long as nothing reads it as published before S6.
5. Warm caches: bytecode cache with jumpdest analysis, speculative warmer (`crates/storage/store.rs:344`,
   `crates/vm/backends/levm/mod.rs:3293`). Applies to contract-heavy chains (Apothem, mainnet), not to
   the transfer-only net2040 ranges, and to `--state.disk`, where reads go to MDBX.
6. RocksDB tuning (write buffers, bloom filters, no compression of state). Does not apply: MDBX is a
   B+tree with no compaction and no memtable. The MDBX equivalents are the page cache, `growth_step` and
   WRITEMAP (ADR-005 open question 3).

## 4. Ranked improvements for xOneRS

| # | Change | Area | Expected gain | Evidence | Safety | Issue |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `ecrecover` on libsecp256k1 (stack context, no global) | import throughput, dense blocks, startup verification | measured: replay CPU -52% (873 to 417 s for 0..20,000); recovery phase 256.7 to 119.1 s | section 2 profile; microbenchmark; ethrex provider.rs:76-97 | Same function as geth's own `secp256k1_ecdsa_recover`; differential test against k256 | #243 |
| 2 | Tx root and receipts on scoped threads beside the state root (blocks of 64 transactions or more) | dense-block execution, commit latency | measured: roots phase of blocks with 64 transactions or more -21% and -17% (101.1 to 79.6 s and 120.5 to 100.5 s, 0..14,000, both builds at the same time, load1 107 and 119); depends on free cores | lib.rs:482-486; ethrex blockchain.rs:1156-1250 | Same pure functions, same inputs | #244 |
| 3 | Each root once in the node path: `run_block` and `BodyVerifier` both compute the tx root, and `verify_receipts` decodes, re-encodes and roots the receipts again | import thread | about half of the roots phase: 2 to 5% | lib.rs:482-484, insert.rs:126, :152, body.rs:283-316 | Every check stays; needs a negative test | #245 |
| 4 | Parallel storage roots and a 16-way account-trie hash (first nibble) | dense-block execution | state root /3 to /8 on blocks with many writes; small on net2040 transfers | mpt.rs:597-660, :409; ethrex blockchain.rs:579-580, CHANGELOG.md:112 | Root is a pure function of the trie | #246 |
| 5 | Pipeline the block commit: execute N+1 while N commits (publish after S6 unchanged) | import throughput, commit latency | the commit time per block (not yet measured on the node; one journal create, write, rename and unlink plus 3 segment appends and one MDBX transaction per block) | mod.rs:525-560, :980-1089; ethrex store.rs:2641-2735 | Needs an ADR-005 amendment and the kill matrix | #247 |
| 6 | Bytecode cache with the analysed `Bytecode`, shared across blocks | contract-heavy execution (Apothem, mainnet) | per ethrex: 30-44 ms to 19-21 ms per block on contract-heavy blocks; 0 on net2040 | overlay.rs:248-257; ethrex CHANGELOG.md:43, :47 | Code is immutable by hash | #248 |
| 7 | jemalloc with a background purge thread | all paths that allocate (trie nodes, diffs, receipts) | ethrex: inline purge was about 12% of block CPU | ethrex ethrex.rs:17-35 | A C build dependency for the release (musl, #193) | #249 |
| 8 | Sender recovery on all cores in `decode_all` (now at most 8 threads and chunks of 256) | replay, producer, any path without the senders pool | blocks of 256 to 2,048 transactions use 1 to 8 threads today | lib.rs:701-730 | Same results, first error by index (test exists) | #250 |

Not filed because an open issue covers it: disk-backed state and persisted path-keyed trie nodes, the
equivalent of ethrex's flat KV and path trie (#145); segment compression, the equivalent of LZ4 on
chain-data column families (#194); resource ceilings (#200); per-phase slow-block timings, which would
have given section 2 without temporary timers (#226); the measurement contract (#13).

## 5. Implemented

Both changes keep execution semantics and every root: same functions, same inputs. Both replays below
are sequential from genesis on the node's own state, `nice -n 19`, on a shared host whose load1 moved
between 37 and 176 during the work; CPU seconds are the stable figure where the change reduces work.

| PR | Issue | Change | Before | After | Replay |
| --- | --- | --- | --- | --- | --- |
| #319 | #243 | `ecrecover` on libsecp256k1, stack context | 873 s CPU for 0..20,000 (three runs, 872.8 to 874.7); 13,000..20,000 in 176 s at load1 42 | 417 s CPU (two runs, 416.0 and 417.3), **-52%**; 13,000..20,000 in 112 to 123 s at load1 50 to 53 | 0..20,000, 0 mismatches, each run |
| #334 | #244 | tx root and receipts on scoped threads beside the state root | roots phase of blocks with 64+ transactions: 101.1 s and 120.5 s | 79.6 s and 100.5 s, **-21% and -17%** (two rounds, both builds at the same time, load1 107 and 119; 91.7 to 89.0 s when run one after the other at load1 108) | 0..20,000 and 0..14,000, 0 mismatches |

Not implemented here: #245 to #250 (ranked above). After #243 the replay profile is sender recovery 56%,
transaction loop 11%, roots 9%, header verification 9%, body checks 8%, state root 6%: #250 (recovery on
every core) and #245 (each root once) are the next two.

## 6. Found on the way

- `xone-replay run --resume` from a checkpoint the same build wrote at 13,000 on net2040 reports a
  stateRoot mismatch at the next block (13,001, an empty block), on `b055158` without any change. A run
  from genesis to 20,000 has 0 mismatches. Every measurement here is therefore from genesis. Filed as #251.
