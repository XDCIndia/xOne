# Performance measurement contract (XA-08, issue #13)

Status: contract only. No benchmark has been run for this document. Every number below is either arithmetic from a cited input or a **target, not result**. Baseline numbers are produced later, on a host the owner assigns, by the procedure in `bench/README.md`.

Sources used: `period: 2`, `epoch: 900` and the V2 `allConfigs` (`minePeriod: 2`, `timeoutPeriod`, `certificateThreshold`) in `crates/xone-chainspec/generated/forks.json` (pin `6fc8e8c2088b7f18264f5663036669d04fa7becf`); issue #1 Revision 1 sections 7 and 8; `docs/reviews/2026-11-10-architecture-scale-pq-review.md` ("What milliseconds and nanoseconds can mean", "Do not confuse rate and latency").

## 1. Rules

1. Nanoseconds and cycles are for selected CPU microbenchmarks only (one primitive, fixed input size, compiler and CPU). They are never an end-to-end requirement.
2. Measure each operation separately, then the whole path.
3. Pin the machine, data, versions and durability mode. A run record without every pinned field in section 2 is invalid.
4. Never compare: RSS with cgroup memory; full state with pruned or anchored state; a tip-following node with a node doing active import; runs with different durability modes; runs on different hosts, heights or retention.
5. Closed-loop-only load is not accepted (section 4).

## 2. Pinned fields (every run record)

| Group | Field |
|---|---|
| Hardware | tier label; CPU model and ISA (flags relevant: avx2, avx512, sha_ni, bmi2); physical cores, logical CPUs, SMT on/off; NUMA nodes and which node the process and the disk sit on; frequency governor, boost on/off, EPP |
| Storage | device model, NVMe or SATA, filesystem and mount options, datadir free space at start, fio-equivalent 4k random read IOPS note (optional) |
| OS | kernel version, cgroup version and limits (memory.max, cpu.max), swap on/off, transparent hugepages |
| Build | profile (release), full `RUSTFLAGS` or Go build flags, binary SHA-256, `xone --version` stamp (or geth/xOneGo version output) |
| Chain | network (Apothem, chainId 51), start height and hash, end height and hash, fork-table pin SHA |
| Node mode | sync mode, prune/retention mode, snapshot or genesis start |
| Durability | MDBX sync mode and flags. The ADR-005 text is not on this branch (`docs/adr` absent), so state: "per ADR-005" and copy the exact flags in force into the record. Geth and xOneGo: record their fsync/ancient/flush settings. Different durability modes are never compared as equal |
| Topology | follower only; peer count and the peer list class (trusted static peers or public); no validator key loaded, no mining |
| Workload | workload id, spec file path and git SHA, `input_hash` (sha256 over the ordered block or transaction inputs), block range or generator seed |
| State | warm or cold (page cache dropped or not; process restarted or not), time since start |
| Load generator | name, version, SHA, open-loop rate schedule, connection and thread counts |

## 3. Metrics

Latency: p50, p95, p99, p99.9 and max, from raw histograms (HDR or the full CSV), with sample count. Also throughput (blocks/s, tx/s, gas/s, requests/s), errors by class, queue depth (max and p99 per queue), CPU (user, sys, per core), I/O amplification, network bytes in and out, conflict retries where applicable.

I/O amplification = device bytes written (from `/proc/diskstats` or `iostat`, sectors x 512) divided by logical bytes of state and chain data committed in the same window. Report read amplification the same way.

**Memory: three different numbers.** They must be reported separately and never compared as one.

| Number | Source | What it includes |
|---|---|---|
| RSS | `/proc/<pid>/status` `VmRSS` (and `RssAnon`, `RssFile`) | Resident pages of the process, including mapped file pages (MDBX mmap) that are resident. |
| cgroup memory | `memory.current`, `memory.stat` | Anonymous memory plus page cache charged to the cgroup, including cache of files the process wrote. |
| Page cache | `memory.stat` `file`, or `/proc/meminfo` `Cached` | Kernel file cache, shared and reclaimable; not owned by one process. |

The A2 footprint table in the plan compared a cgroup figure with RSS. It must be re-measured with identical semantics before use in any comparison.

## 4. Load model

- Open-loop: requests or blocks are issued on a schedule independent of response time. Latency is measured from the **intended** send time, so queueing delay is counted (coordinated-omission-aware). A load generator that waits for a response before sending the next request is closed-loop and is rejected, except for the sequential block-replay workloads where the unit is "import this block range"; those report per-block import latency and are labelled replay.
- Three load levels per workload that has a rate: below saturation (about 50 percent of the measured knee), near (about 90 percent), above (about 120 percent). The knee is found by a ramp run first and recorded. Above saturation the pass criterion is bounded queues and recovery, not latency.
- Soak: at least 1 h at the near-saturation level for the baseline, longer for endurance claims.
- Repeat rule: three runs per setup, each from the same declared state. Report all three values and the spread (max minus min, and as a percentage of the median). Pass: the three agree within the tolerance in the workload spec (`tolerance_pct`, default 10 percent for p50, 20 percent for p99; owner may change). If not, find the noise source, fix the setup, rerun (issue #13 loop). Record the load generator's own limits (its max rate with a null target).
- The first run after a restart is cold and is reported separately from warm runs.

## 5. Workloads

Machine-readable specs are in `bench/workloads/`. Summary:

| Id | What it exercises |
|---|---|
| transfers | plain value transfers, admission, signature recovery, balance updates |
| evm-heavy | compute-bound contract calls |
| storage-heavy | many SLOAD/SSTORE on large storage |
| hot-account-conflict | many transactions touching the same accounts (conflict and ordering cost) |
| epoch-switch | 20 consecutive V2 epoch switches including reward and penalty computation |
| reorg | side-branch import, switch of canonical head, unwind and re-execute |
| malformed-traffic | invalid blocks, transactions and messages; cost of rejection; isolation from the valid path |
| rpc-sync-interference | RPC read load and serving peers while importing; effect on import latency |

Block ranges are Apothem archive ranges chosen from the fork table. Boundaries used: V2 switch 56,828,700; EIP-1559 71,550,000; Cancun 71,551,800; Prague 84,333,000. Where a range is nominal (epoch switches under V2 are by round, so the block number is not always an exact multiple of 900) the spec says the range is resolved against the archive and the resolved hashes are written to the run record.

## 6. Latency budgets (target, not result)

Input: slot = 2 s. Source: `xdpos.period: 2` and `minePeriod: 2` in the chainspec fork table. All values are **targets, not results**. The margins are proposals for the owner to confirm; they are not measured.

Arithmetic. The slot is 2,000 ms. Reserve first, spend the rest:

| Line | ms | Basis |
|---|---|---|
| Slot | 2,000 | chainspec `period` |
| Propagation margin (block to peers, vote back) | 500 | proposal: 25 percent of slot, to be replaced by a WAN measurement |
| Signing margin (durable signing intent, signature, send) | 100 | proposal: 5 percent of slot |
| Headroom (jitter, GC or compaction pauses, clock skew) | 200 | proposal: 10 percent of slot |
| Processing budget, p99 | 2,000 - 500 - 100 - 200 = **1,200** | derived |

Processing budget split (the import/build path: queue + validation + execution + root + durable commit):

| Operation | p99 target (ms) | What is inside | Measured how |
|---|---|---|---|
| Decode and admission | 50 | RLP decode, size and field limits, queue wait | per-stage timer, valid and invalid inputs separately |
| Cryptographic checks | 150 | tx sender recovery, header seal, QC/TC signature verification | per-signature microbenchmark (ns allowed here only) plus per-block stage timer |
| State access | 300 | cold and warm reads and trie node loads not nested in execution | per-block read count, bytes and stage timer |
| Execution | 450 | EVM and system transactions, gas accounting | per-block timer, split by workload |
| Root computation | 150 | state, receipts and transactions roots | per-block timer |
| Durable commit | 100 | write set commit under the pinned durability mode, including publication step | per-block timer and fsync count |
| Sum | 50+150+300+450+150+100 = **1,200** | equals the processing budget | |
| Propagation | 500 | outside processing; a margin, measured on a multi-node WAN-like deployment | multi-node run only |
| Finality | see below | consensus rule, not a processing stage | multi-node run only |

Finality: under V2 a block is committed after a quorum certificate chain over consecutive rounds (3-chain rule; confirm against the consensus spec page when it lands in `spec/`). With no timeouts and one block per 2 s round, that is at least 3 x 2 s = 6 s from proposal. Target: committed within 3 slots plus the propagation margin, about 6.5 s, in the no-fault case. A round timeout (`timeoutPeriod` in the fork table, 10 s in later Apothem configs, 60 s at round 0) pushes finality past any slot-based figure; fault cases are reported separately.

Budget rules: the split is a starting allocation. After the baseline the owner may move budget between lines; the sum must stay at or below 1,200 ms and the reserves must not shrink without a measured reason. Stages are measured per block, p99 over the run, so the sum of stage p99s is conservative (the p99 of the sum is lower). A failure of the sum is judged on the whole-path p99, with stage numbers used to find the cause.

RPC budget (separate boundary): a warm local read p99 under 10 ms is a proposed goal from the review, not an observed xOne result.

Rate is not latency: 1 / 3,000 TPS = about 333 us mean arrival interval; at a 2 s block, 3,000 TPS is about 6,000 transactions per block, subject to gas and payload limits (arithmetic, not a result).

## 7. Producer qualification by eligible turns

Replaces "1,800 blocks, 100 percent of turns" (G5).

**Eligible turn.** A round in which the producer under test is the scheduled leader for that round (from the leader-selection rule applied to the epoch's validator list), is in the validator set for the epoch, is not penalised or excluded for that epoch, and the node is running, synced to the tip and has a valid signing key and safety record at the start of the round. A turn skipped because the node was deliberately stopped by the test (restart, fault injection) is counted under that fault case, not dropped silently. Turns where the node was ineligible are listed with the reason and excluded from the denominator.

**Miss.** An eligible turn in which no block from the producer is in the canonical chain for that round. Every miss gets a record: round, block height, wall time, cause (one of: node down, not synced, signer unavailable, build too slow, propagation, vote or timeout path, peer or network, upstream fault, unknown) and evidence (log lines, timestamps). "Unknown" is a failing state until resolved.

**Counted quantities.** Eligible turns, elapsed wall time, epoch changes crossed, restarts (planned kills and unplanned), fault cases injected and their outcomes, misses with causes.

**Proposed minimums (Revision 1, section 7): 200 eligible turns, 72 h, 3 epoch changes, 5 restarts. Pending owner confirmation.** Not decided here. All four must hold, together with zero unexplained misses; the pass rule for explained misses is the owner's decision.

**Why 1,800 blocks is not enough.** Arithmetic:

- 1,800 blocks / 97 validators = 18.56 turns per validator (about 18-19; with 96 it is 18.75). Source of the validator count: issue #13 ("about 96-97 masternodes"); the set size is read from the epoch header at run time, and Apothem V2 allows up to 108 in the chainspec.
- The tracker's window: 1,078 blocks gave 11-12 turns (1,078 / 94 = 11.5), consistent with the same arithmetic. Issue #1 Revision 1 quotes 17-20 turns per client for the window.
- With n eligible turns and 0 misses, the one-sided 95 percent upper bound on the miss rate p solves (1-p)^n = 0.05, so p = 1 - 0.05^(1/n), about 3/n (rule of three). n = 18: 15.3 percent exact (16.7 percent by 3/n). n = 19: 14.6 percent. n = 200: 1.49 percent (3/n = 1.5 percent).
- So a clean 1,800-block run only shows the miss rate is below about 15 percent with 95 percent confidence. It cannot support a claim such as 99 percent reliability. Reaching a 1.5 percent bound needs about 200 turns, which is the proposed minimum. Zero misses is never reported as a reliability guarantee.
- Time cost: with about 97 validators and 2 s rounds one validator gets a turn about every 194 s. 200 turns is about 38,800 s = 10.8 h only if no rounds are lost; the 72 h minimum is for epoch changes, restarts and fault coverage, not for the turn count alone. Epoch changes: 900 blocks x 2 s = 1,800 s = 30 min per epoch, so 3 epoch changes occur within 1.5 h; they are in the minimums for the cases (penalty, reward, set change) they cross, not for time.

## 8. What this document does not contain

No baseline numbers, no host assignment, no measured margins. The baseline of geth-XDC and xOneGo on one pinned read-only follower host is a procedure in `bench/README.md`, to be run when the owner assigns a host. No validator, producer host, key or fleet host is used.

## 9. Owner decisions

1. Assign the dedicated baseline host (and its hardware tier label).
2. Confirm or change the mint minimums (200 turns, 72 h, 3 epoch changes, 5 restarts) and the pass rule for explained misses.
3. Confirm the margins (propagation 500 ms, signing 100 ms, headroom 200 ms) and the stage split.
4. Confirm run tolerances (`tolerance_pct`) and the saturation levels (50/90/120 percent).
5. Confirm the sync/prune mode to baseline (same for both clients) and the durability mode on each.
6. Approve the Apothem block ranges once resolved against the archive, and where the input blobs are stored.
