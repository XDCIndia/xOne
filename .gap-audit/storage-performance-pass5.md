# Disk and execution-performance audit — pass 5

Reviewed xOneRS at `54d8ae7da15c87dd77a7a86ca695c813bad5ec91` on 2026-11-14. This pass extends the prior 50-issue audit with **eight distinct issues**, all created and recorded immediately in the ledger. Application code is unchanged. No node benchmark, deployment, production disk inspection or claimed speedup was performed.

## New work

| Gap | Priority | Shippable unit | GitHub |
| --- | --- | --- | --- |
| GAP-0110 | P1 | Prune transaction lookup rows with their retained history | [#326](https://github.com/XDCIndia/xOneRS/issues/326) |
| GAP-0111 | P1 | Bound segment writer handles across chunk rotation | [#327](https://github.com/XDCIndia/xOneRS/issues/327) |
| GAP-0112 | P1 | Stream segment recovery and quarantine with bounded buffers | [#328](https://github.com/XDCIndia/xOneRS/issues/328) |
| GAP-0113 | P1 | Bound temporary memory during range deletion and account storage removal | [#329](https://github.com/XDCIndia/xOneRS/issues/329) |
| GAP-0114 | P1 | Apply disk-space admission before bootstrap and all block writers | [#330](https://github.com/XDCIndia/xOneRS/issues/330) |
| GAP-0115 | P2 | Make optional preimage retention configurable while preserving wide balances | [#331](https://github.com/XDCIndia/xOneRS/issues/331) |
| GAP-0116 | P2 | Store compact receipts without redundant per-receipt blooms | [#332](https://github.com/XDCIndia/xOneRS/issues/332) |
| GAP-0117 | P2 | Add a bounded persistent log index for selective history queries | [#333](https://github.com/XDCIndia/xOneRS/issues/333) |

Five P1 findings concern existing resource or disk-admission lifecycles. Three P2 findings add optional storage/query capabilities with explicit measurement gates. These are source-supported risks and gaps, not reports of observed production failures.

## What the ethPandaOps comparison means

The requested [Engine API Timings page](https://lab.ethpandaops.io/ethereum/execution/timings) compares Ethereum Engine API requests. Its `newPayload` view is not a measurement of XDC finality, standalone EVM time, database size or durable producer throughput. A search-index snapshot was available, but the live client-rendered page did not expose readable data through retrieval. No current leaderboard values are copied into this report.

The reviewed source supports these useful methods:

| Method | Evidence | xOneRS application |
| --- | --- | --- |
| Separate client/version and validation outcome | [ethpandaops/lab: `src/pages/ethereum/execution/timings/components/NewPayloadTab/NewPayloadTab.tsx`](https://github.com/ethpandaops/lab/blob/9f4f88fbe08909fc14b0bcccd4f6d29312a07eff/src/pages/ethereum/execution/timings/components/NewPayloadTab/NewPayloadTab.tsx) | Record build SHA, operation and accepted/rejected/failed outcomes separately. Do not make rejected work look like fast successful execution. |
| Compute range-level VALID percentiles from a dedicated endpoint | [ethpandaops/lab: `src/pages/ethereum/execution/timings/hooks/useEngineTimingsData.ts`](https://github.com/ethpandaops/lab/blob/9f4f88fbe08909fc14b0bcccd4f6d29312a07eff/src/pages/ethereum/execution/timings/hooks/useEngineTimingsData.ts) | Keep raw per-block samples and compute percentiles per cohort. Never average per-client/hourly percentiles into a purported global percentile. |
| Default reference-node filter with an explicit node class | [ethpandaops/lab: `src/pages/ethereum/execution/timings/IndexPage.tsx`](https://github.com/ethpandaops/lab/blob/9f4f88fbe08909fc14b0bcccd4f6d29312a07eff/src/pages/ethereum/execution/timings/IndexPage.tsx) and the hook above | Pin hardware and topology. Reference-node selection does not imply that every displayed node has identical hardware. |
| Named reference hardware groups | [ethpandaops/lab: `src/constants/eip7870.ts`](https://github.com/ethpandaops/lab/blob/9f4f88fbe08909fc14b0bcccd4f6d29312a07eff/src/constants/eip7870.ts) | Separate host classes; record CPU, RAM, device, filesystem, NUMA and power settings. |
| Isolated scenario execution and resource collection | [ethpandaops/benchmarkoor: `README.md`](https://github.com/ethpandaops/benchmarkoor/blob/32a073958e5f031284e59892316308623227135d/README.md), [ethpandaops/benchmarkoor: `docs/configuration.md`](https://github.com/ethpandaops/benchmarkoor/blob/32a073958e5f031284e59892316308623227135d/docs/configuration.md), [ethpandaops/benchmarkoor: `pkg/stats/cgroup_reader.go`](https://github.com/ethpandaops/benchmarkoor/blob/32a073958e5f031284e59892316308623227135d/pkg/stats/cgroup_reader.go) | Build a native XDC replay/follower harness under #13. Do not feed Ethereum Engine payloads to XDPoS and call the operations equivalent. |
| Structured execution-log collection | [ethpandaops/xatu: `docs/sentry-logs.md`](https://github.com/ethpandaops/xatu/blob/dc6d4e30fe2961fb8afefa6a2828dbaae842bf78/docs/sentry-logs.md) | Reuse the phase fields owned by #226 and the metrics/export work in #41. |

The relevant existing owners are [#13](https://github.com/XDCIndia/xOneRS/issues/13) for the measurement contract and runner, [#226](https://github.com/XDCIndia/xOneRS/issues/226) for per-block phases, [#41](https://github.com/XDCIndia/xOneRS/issues/41) for live observability, and [#197](https://github.com/XDCIndia/xOneRS/issues/197) for traffic replay. No duplicate issue was created for these capabilities. Ethereum Engine compatibility remains a separate profile in [#214](https://github.com/XDCIndia/xOneRS/issues/214).

## Disk optimization order

1. **Correct storage lifecycles:** implement #326–#330. A smaller datadir is not useful if pruning leaks index rows, archive imports exhaust handles, or recovery needs memory proportional to a corrupt tail.
2. **Establish an apples-to-apples baseline:** use #13 and #149. Measure full versus minimal retention separately; do not compare an anchored snapshot to a full historical database as if it proved a better engine.
3. **Remove verified duplication and improve layout:** #135 and #149 already own changeset placement, repeated state/trie records and ordered writes. #176 owns inspection/compact-copy maintenance, #177 retention, #194 frozen-segment compression, and #225 separate history storage.
4. **Measure independent optional improvements:** #331 preimage policy, #332 compact receipts, #333 log index. Report index write/storage cost and receipt reconstruction CPU alongside savings. Preserve wide-balance addresses and XDC byte equivalence.
5. **Improve execution with the same contract:** #259 prefetch, #244/#245 root work, #246 parallel trie work, #247 execution/commit overlap, #248 bytecode analysis cache, #249 allocator choice, #250 sender recovery, and #260 speculative EVM execution. Do not relax durable producer semantics to obtain a lower number.

## Required baseline artifacts

Existing [XDCIndia/xOneRS: `bench/README.md`](https://github.com/XDCIndia/xOneRS/blob/54d8ae7da15c87dd77a7a86ca695c813bad5ec91/bench/README.md) and [XDCIndia/xOneRS: `docs/perf.md`](https://github.com/XDCIndia/xOneRS/blob/54d8ae7da15c87dd77a7a86ca695c813bad5ec91/docs/perf.md) already define most of the contract. The following is the concrete disk/timing extension to execute under those issues, not a replacement benchmark specification.

| Artifact | Required content |
| --- | --- |
| Cohort manifest | Chain ID, genesis/fork pin, start/end height and hash, dataset hash, client/version/SHA, binary hash, build flags, operation, retention, state mode, durability, host/device/filesystem and warm/cold state. |
| Per-block trace | Decode/admission, header/QC verification, sender recovery, EVM execution, state reads, root calculation, segment append, MDBX commit, explicit sync and end-to-end time; record queueing separately. |
| Distributions | Sample count, p50/p95/p99/p99.9/max, throughput, failures and timeouts. At least three repeats with variance and load-generator limits; preserve raw data. |
| Disk inventory | Logical key/value and segment bytes, MDBX live/reusable/allocated pages, apparent file length and filesystem-allocated bytes, receipt/body/history floors, index coverage, snapshots/checkpoints and temporary/quarantine peak bytes. |
| Resource trace | RSS, page cache and cgroup memory as separate quantities; CPU, device read/write bytes, IOPS, queue depth, sync latency, open file count, and concurrent read transaction lifetime. |
| Amplification | Report device write bytes divided by logical committed bytes with the exact denominator. For queries, report bytes/blocks read per returned log and per candidate. Separate background work from foreground latency. |
| Correctness record | Roots, receipts, canonical hashes, retention-boundary errors, reorg/unwind results, crash points, restart state and producer durability. |

Workloads must include realistic XDC block replay, storage-heavy/churn blocks, repeated hot-account writes, sparse/broad log filters, chunk rotation, repeated prune cycles, wide-balance restart/SNAP cases, epoch/reorg boundaries, and malformed/large recovery tails. Use both cold and warm reads. Maintenance experiments must record peak temporary disk use: compact-copy, backup and quarantine can need space before they reclaim it.

An MDBX deletion can free pages for reuse without reducing the file's apparent length. Do not label reusable-page reclamation as reclaimed filesystem bytes. A log index may improve query latency while increasing total storage. General compressed-file ratios do not establish compact-receipt savings; test the four combinations separately.

No dedicated baseline host was assigned in this task and no performance run was attempted. The current deliverable is the audit and implementation backlog.

## Competitor comparison evidence

| Project | Reviewed design evidence | Applicable decision |
| --- | --- | --- |
| Geth | [ethereum/go-ethereum: `core/types/receipt.go`](https://github.com/ethereum/go-ethereum/blob/8059ff8beb3170eb8b977169ac51bb0423c97ee1/core/types/receipt.go); [ethereum/go-ethereum: `core/filtermaps/indexer.go`](https://github.com/ethereum/go-ethereum/blob/8059ff8beb3170eb8b977169ac51bb0423c97ee1/core/filtermaps/indexer.go); [ethereum/go-ethereum: `core/rawdb/freezer_table.go`](https://github.com/ethereum/go-ethereum/blob/8059ff8beb3170eb8b977169ac51bb0423c97ee1/core/rawdb/freezer_table.go) | Compact receipts, coverage-aware log indexing and bounded recovery reads inform #328/#332/#333. |
| Reth | [paradigmxyz/reth: `crates/prune/prune/src/segments/user/transaction_lookup.rs`](https://github.com/paradigmxyz/reth/blob/36600d5ff9628465475dd4c7608e2e9f2fe24273/crates/prune/prune/src/segments/user/transaction_lookup.rs); [paradigmxyz/reth: `crates/storage/provider/src/providers/static_file/writer.rs`](https://github.com/paradigmxyz/reth/blob/36600d5ff9628465475dd4c7608e2e9f2fe24273/crates/storage/provider/src/providers/static_file/writer.rs); [paradigmxyz/reth: `crates/storage/db-api/src/cursor.rs`](https://github.com/paradigmxyz/reth/blob/36600d5ff9628465475dd4c7608e2e9f2fe24273/crates/storage/db-api/src/cursor.rs) | Explicit transaction-index pruning, writer rotation and mutable cursors inform #326/#327/#329. |
| Erigon | [erigontech/erigon: `execution/state/rw_v3.go`](https://github.com/erigontech/erigon/blob/5b59b99c73893b4a21cc1cea175e6263cf914912/execution/state/rw_v3.go); [erigontech/erigon: `db/kv/tables.go`](https://github.com/erigontech/erigon/blob/5b59b99c73893b4a21cc1cea175e6263cf914912/db/kv/tables.go); [pruning modes](https://docs.erigon.tech/fundamentals/pruning-modes) | Address/topic indices and independently described history retention reinforce the need to measure query coverage and disk tradeoffs. Current public docs and older README prose disagree about default receipt storage; this report uses code for index presence and version-specific docs for mode descriptions. |
| Nethermind | [NethermindEth/nethermind: `src/Nethermind/Nethermind.State.Flat/PersistedSnapshots/Sorted/FORMAT.md`](https://github.com/NethermindEth/nethermind/blob/a7dd73052e696ad934d228aad3f33e660701fdb7/src/Nethermind/Nethermind.State.Flat/PersistedSnapshots/Sorted/FORMAT.md); [state pruning](https://docs.nethermind.io/next/fundamentals/state-pruning/) | Sorted snapshot layout and pruning headroom are comparison inputs to #135/#149; do not copy another chain's free-space constants. |
| Besu | [besu-eth/besu: `ethereum/core/src/main/java/org/hyperledger/besu/ethereum/trie/pathbased/bonsai/storage/cache/Versioned-cache-doc.md`](https://github.com/besu-eth/besu/blob/2b839d96863d01210f45a6f62127e6229e5b0889/ethereum/core/src/main/java/org/hyperledger/besu/ethereum/trie/pathbased/bonsai/storage/cache/Versioned-cache-doc.md) | Version-pinned cross-block cache behavior is relevant to existing bounded-state/reorg work in #145, rather than a new cache issue. |
| ethrex | [lambdaclass/ethrex: `docs/perf/README.md`](https://github.com/lambdaclass/ethrex/blob/7e272ac5075aec832304909b376bada4eca52e77/docs/perf/README.md); [lambdaclass/ethrex: `docs/perf/methodology.md`](https://github.com/lambdaclass/ethrex/blob/7e272ac5075aec832304909b376bada4eca52e77/docs/perf/methodology.md) | Reproducible experiments and retained run artifacts belong under #13. No benchmark instructions from competitor documentation were executed. |
| Seismic Reth | [SeismicSystems/seismic-reth: `README.md`](https://github.com/SeismicSystems/seismic-reth/blob/faa3a30c430676dfc2d0224fe2bcce18bf0e1e88/README.md); [SeismicSystems/seismic-reth: `docs/seismic/features.md`](https://github.com/SeismicSystems/seismic-reth/blob/faa3a30c430676dfc2d0224fe2bcce18bf0e1e88/docs/seismic/features.md) | Newly selected at 68/100. Its applicable Reth-derived storage mechanisms corroborate existing work; shielded storage/transactions and token gas rules are explicitly skipped as XDC protocol conflicts. |

## Rescan and rejected hypotheses

The selected-client document inventory is in `rescan-pass5.json`. Source matches are discovery leads, not automatic claims that an entire production feature exists. The new Seismic client received a separate module/capability map in `feature-matrix.md` and `module-map-pass5.md`.

- Clean-close group-commit flush is **present**, via `Core::drop -> NodeStore::steady`; no issue was filed for its absence. The configured interval is explicitly commit-driven.
- Compression, backups, storage metrics, read caches, parallel execution and benchmark automation already have owners listed above. Different competitor implementations are not automatically separate product gaps.
- Seismic's alternate signed-body authenticator is an authentication design option, not proof that the authenticated-operator capability in #60 needs a duplicate issue.
- Carmen stayed license/docs-only. Its BUSL grant is Sonic-specific. Paprika is archived. No new database engine is recommended without comparable evidence.
- The audit does not import Ethereum or Seismic transaction/fork/consensus rules into XDC.

Final closure counts and the last expansion pass are recorded in `REPORT.md` and `discovery-pass5.json`. Closure means no newly untracked unit was found in the recorded pass; it does not mean the issues are implemented or that the codebase is defect-free.


## Latest-main reconciliation

While the audit ran, main advanced to `5af188dbeb6199a859e9823720a9c8df689fcb87`. Its diff from the audit pin was inspected. The eight storage/disk findings remain supported; the config/flag changes add RPC JWT authentication and redaction rather than preimage retention.

Main also gained [the ethrex storage/execution comparison](https://github.com/XDCIndia/xOneRS/blob/5af188dbeb6199a859e9823720a9c8df689fcb87/docs/perf/ethrex-storage-comparison.md), which records other contributors' shared-host replay measurements and work on #243/#244. Those are existing repository results, not measurements from this audit or a controlled comparison against the Lab dashboard. This report does not supersede them or recreate #243–#250.
