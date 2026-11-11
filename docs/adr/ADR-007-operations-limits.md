# ADR-007: Operations limits (bounded queues, priority isolation, budgets, logs, disk)

Status: proposed (Gate C design). Issue: XDCIndia/xOne#16 (XA-11). Plan: issue #1 ADR-007 line, R3, K2, A4. Budgets: `docs/perf.md` section 6. Storage: `docs/adr/ADR-005-storage-and-publication.md` section 9.
Code: `crates/xone-primitives/src/limits/` (the primitives, re-exported by `crates/xone-node/`) and `crates/xone-node/src/services/` (the wiring). Tests: `tests/ops/PLAN.md`.

Tags: **[D]** a decision of this ADR, a proposal, no test has run. **[U]** unverified. Every number is a **proposal for the owner**, not a measured value. Nothing here is tuned (issue non-goal).

## 1. Goals and evidence

1. Every queue has a bound and a named overflow action.
2. Consensus work runs before RPC, sync and serving work, and a flood of those cannot move the round timer or the vote path outside their budget.
3. Logs have a bytes/s limit. No INFO on a per-message path.
4. Low disk stops import with one alarm, then exits cleanly. It never reaches a torn write.
5. A peer that sends bad or oversized messages is dropped and banned.

Evidence behind each: an INFO line on served `xdpos2` header requests flooded syslog (go-ethereum#1953, fixed by logging at debug); a full disk halted a network (plan A4.8); xOneGo's miner waited 80 s on a state re-execution and lost 40% of the slots on net5151 (plan A5, K2; PRs 674, 708, 732); unbounded queues and lost wake-ups are named in K2.

## 2. Queue inventory

Rule: a queue with several producers on one peer socket must never use **Backpressure** (it would stop reading the socket and delay a vote behind a block body on the same connection). Peer-facing queues use drop, reject or disconnect. Backpressure is used only where the producer is our own stage.

Bounds are in items and bytes; the queue holds at most both. "Count" means each overflow increments `queue_overflow_total{queue}`; one WARN per queue per 10 s at most (section 6).

| # | Queue | Scope | Bound [D] | Overflow | Why |
|---|---|---|---|---|---|
| Q1 | Inbound Vote / Timeout / SyncInfo | per peer | 256 msgs, 256 KiB | DropOldest | A round has at most about 108 votes and 108 timeouts. A vote for an old round is worth nothing, the newest is the useful one. |
| Q2 | Same, after dedupe, into the consensus thread | global | 4,096 msgs | DropOldest | One consumer (P0). Quorum arrives from many peers, so losing the oldest costs no liveness. Stale-round messages are discarded at dequeue. |
| Q3 | Inbound block announce, new-block | per peer | 16 msgs | Reject, score `RateExceeded` | An honest peer sends a few per slot. |
| Q4 | Live head blocks awaiting verification | global | 16 blocks, 32 MiB | DropOldest | Newest head matters. A dropped block is refetched by sync as a gap. |
| Q5 | Inbound tx gossip (Transactions, PooledTransactions) | per peer | 128 txs, 1 MiB | Reject | Txs are fungible. Volume alone is not misbehaviour, so no score. Sustained excess is caught by the peer token bucket (Q10). |
| Q6 | Tx pool admission (decode, sender recovery) | global | 4,096 txs, 8 MiB | Reject | Pool size and eviction are a separate work item. This is only the intake. |
| Q7 | Inbound requests we must serve (GetBlockHeaders, Bodies, Receipts, snap) | per peer | 8 requests | DisconnectPeer | The wire protocol has no "busy" reply. A peer past 8 outstanding is flooding. |
| Q8 | Header import (sync) | global | 2,048 headers | Backpressure | Producer is our downloader. It stops requesting. |
| Q9 | Body and receipt import (sync) | global | 128 blocks, 64 MiB each | Backpressure | Same. The byte bound is what protects memory, the item bound is for tiny blocks. |
| Q10 | Verified blocks to execution and commit (P1 input) | global | 64 blocks | Backpressure | Producer is our verifier. Execution is the slow stage by design. |
| Q11 | Execution to durable commit (DB writer) | global | 2 blocks | Backpressure | At most one commit in flight, one prepared. A deeper queue only widens the unwind window. |
| Q12 | RPC HTTP: accepted connections / admission wait / in flight | per transport | 256 conn / 128 waiting / 64 running | Reject (429, `-32005`) | See section 4. |
| Q13 | RPC WebSocket: connections / in flight per conn / subscription out-buffer | per transport | 128 / 8 / 1,024 msgs | Reject; slow consumer: DisconnectPeer (drop the subscription) | A slow subscriber must not hold memory. |
| Q14 | RPC IPC: connections / in flight per conn | per transport | 16 / 8 | Reject | Local, trusted, still bounded. |
| Q15 | Snapshot serving (snap/1 ranges, snapshot export) | global | 4 sessions, 1 per peer, 16 MiB/s | Reject (reply empty and score `RateExceeded`; repeated: disconnect) | P3. It must never compete with import for disk bandwidth. |
| Q16 | Pruning jobs | global | 1 job, new floor replaces the pending one | Coalesce (drop the older job) | ADR-005 section 10 prunes whole chunk files; one floor is all that matters. |
| Q17 | Log sink (lines to the writer thread) | global | 8,192 lines | Reject (drop newest), counted, summarised | Hot threads never block on logging. Alarms use a reserved lane (section 6). |
| Q18 | Peer-manager events (connect, disconnect, ban) | global | 1,024 | DropOldest | Control plane. A lost event is repaired by the next periodic sweep. |

Not queues: the round timer (a deadline, section 3), the durable signing intent (a synchronous call into the safety store, ADR-010), the DB commit (ADR-005).

Per-peer memory is bounded by the sum of the per-peer rows: about 256 KiB + 1 MiB + 8 small requests, so under 1.5 MiB. With a 50-peer limit [D] that is under 75 MiB. The global rows add about 150 MiB. These are arithmetic from the table, not measurements.

## 3. Priority classes and enforcement

| Class | Work | Runs on |
|---|---|---|
| P0 | Round timer, vote and timeout handling, vote signature check, durable signing intent, QC/TC assembly | One dedicated thread (`consensus`) and a 2-thread dedicated verify pool for P0 signatures |
| P1 | Block verification (seal, QC), execution, state root, durable commit | One dedicated thread (`exec`) and a verify pool (rayon) sized to leave the P0 cores free |
| P2 | Sync: header, body and receipt download and their import | Async tasks (network) feeding Q8 to Q10 |
| P3 | RPC (all transports), traces, snapshot serving | Async accept and parse; execution of RPC reads on a separate low-priority pool |
| P4 | Pruning | One background thread |

Enforcement, all of it structural and none of it a hint:

1. **Threads, not tasks, for P0 and P1** (plan R3, K2). The consensus and execution threads are synchronous, own their state, and are not on the async runtime. The async runtime does network and RPC I/O only. A blocked runtime therefore cannot stop a timer.
2. **No shared lock between P0 and anything lower.** P0 reads state only from the last committed snapshot and from its own memory. It never calls state re-execution and never waits on a cold state load (the xOneGo 80 s lesson, plan A5): P1 prefetches, P0 builds only on an already-executed parent. A P0 path that would need a cold read takes the miss as a missed proposal and logs it, instead of waiting.
3. **Bounded channels everywhere** (section 2). A P0 queue is never behind a P2 or P3 producer: Q1 and Q2 are separate from Q8 to Q10.
4. **CPU reservation.** Threads of P2 to P4 together are at most `cores - 2`; the pools of P0 and P1 are fixed at start. The standard library cannot set thread priority and `unsafe` is forbidden, so priority between classes is by thread count and by the unit's cgroup (`cpu.weight` favouring the node over the host, and `AllowedCPUs` pinning P0 if the owner wants it). **ponytail:** no per-thread niceness; add it only if the flood test shows CPU contention that core reservation does not fix.
5. **Read-transaction deadline.** An RPC read holds an MDBX reader. A long reader pins old pages and grows the file while import continues. Every RPC read has a deadline (section 4) and the reader is dropped at it. This is the shared resource most likely to couple RPC to import [U: MDBX reader behaviour is from ADR-005 reading, not measured here].
6. **Load shedding.** When the watchdog reports P0 lag above its warning level, RPC cost classes C2 and C3 and snapshot serving are rejected until it clears. The flood test must show this switch is what keeps the budget.
7. **Watchdog on the round timer.** The `consensus` thread stores a heartbeat (monotonic ms) at the top of each loop. A watchdog thread, separate from the runtime, compares it with the round deadline every 50 ms [D]:

| Lag past the deadline [D] | Action |
|---|---|
| over 100 ms | WARN once, metric `round_timer_lag_ms`, start load shedding (item 6) |
| over 2 s (one slot) | ERROR alarm once, tell the operator the node is not producing |
| over 10 s | Fail-stop: exit non-zero. The restart reads the safety record (ADR-010), so a restart cannot double-sign |

The heartbeat is a field of the node object, not a process-global.

**Budgets under flood (target, not result).** `docs/perf.md` section 6 gives the processing budget (1,200 ms) and a 100 ms signing margin and a 200 ms headroom, but no figure for the vote path or the timer. Proposal: the round timer fires with p99 lag at or under 50 ms, and the vote path (receive, verify, durable intent, send) has p99 at or under 100 ms (the signing margin), both under RPC and sync flood, and at most 1.25 times the idle p99. Owner to confirm (question 1).

## 4. RPC quotas

Classes by cost, assigned per method in a table in code, default C1 for an unknown method [D]:

| Class | Examples | Concurrency (global) | Deadline | Other limits |
|---|---|---|---|---|
| C0 cheap | `eth_chainId`, `eth_blockNumber`, `net_version`, `web3_clientVersion` | within the global 64 | 2 s | |
| C1 state read | `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_getBlockBy*`, `eth_getTransactionBy*`, `eth_call` with a 50M gas cap | 32 | 10 s | |
| C2 heavy | `eth_getLogs`, `eth_estimateGas`, `eth_getBlockReceipts`, `eth_feeHistory` | 8 | 10 s | `getLogs`: 5,000-block range, 10,000 logs, 20 MiB response |
| C3 trace | `debug_trace*`, `trace_*` | 2, and 1 per connection | 30 s | Off on the public listener by default. P3. |

Common limits [D]: request body 5 MiB; batch up to 100 items and the batch counts as the sum of its parts for cost; response cap 20 MiB; per-connection in flight 8.

**Token bucket.** One per connection, in cost units: C0=1, C1=5, C2=20, C3=100; sustained 200 units/s, burst 400. A second bucket for the whole transport. `TokenBucket` in `xone-node` is the primitive.

**Cancellation.** When the socket closes (HTTP client gone, WS dropped, IPC closed), the request is cancelled: the async side drops the future and sets a per-request cancel flag that the sync worker checks at every transaction boundary and every 4,096 EVM steps in traces and calls. A cancelled worker releases its MDBX reader at once. The deadline above is the same mechanism. The EVM-step hook does not exist yet (`tests/ops/PLAN.md`).

**Backpressure signal.** Over limit: HTTP 429 with `Retry-After` (seconds until the bucket holds the cost); JSON-RPC error `-32005` "limit exceeded" with the same value in `data`. WebSocket: stop reading from that connection while its in-flight count is full. Under load shedding the reply is `-32005` with `data: "node busy"`.

RPC reads continue during `StopImportAlarm` (ADR-005: reads continue while degraded).

## 5. Peer DoS budget

Size caps by class [D, to be set against geth-XDC `eth/protocols/eth/protocol.go` limits before coding; not checked here]:

| Class | Hard cap |
|---|---|
| Vote, Timeout | 1 KiB |
| SyncInfo | 32 KiB (two certificates of up to 108 signatures of 65 bytes, plus headers) |
| Status, Hello | 4 KiB |
| Single transaction | 128 KiB |
| Any other eth message | 10 MiB |

Score (`PeerBudget`), proposals:

| Offence | Points |
|---|---|
| Malformed (does not decode) | 10 |
| Invalid (decodes, fails a rule: signature, header) | 25 |
| RateExceeded (queue or bucket overflow) | 5 |
| Oversized (above the class cap) | 100 |

Threshold 100: disconnect and ban. Decay 10 points per minute. Ban 30 minutes. So 10 malformed messages inside a short window, or one oversized message, ban the peer. A peer that errs once every 2 minutes never reaches it. The oversized check runs on the frame length before the body is read, so no allocation follows the offending header.

Limits of this design: a score per peer does not stop many peers. That is held by the global rows of section 2 and a limit on inbound connections per /24 [D]. Trusted and static peers: disconnect and back off but no ban, decided in the peer manager (question 5). Ban table and escalation for repeat offenders are not in the primitive (ponytail in `peer.rs`).

## 6. Log budget

Rules:

1. No INFO on a per-message path. Per-message means per peer message, per tx, per vote, per header request, per RPC request. Those log at DEBUG or TRACE, off by default, and DEBUG is rate-limited too (go-ethereum#1953).
2. INFO is for per-block events (about 0.5 lines/s at one block per 2 s), state changes and start-up.
3. Every subsystem has a `TokenBucket` in bytes. ERROR and alarm lines draw from a separate reserved bucket so that a flood of INFO cannot hide an alarm.
4. A suppressed line is counted. At most one summary line per subsystem per 10 s: `suppressed lines=N bytes=M subsystem=S`.
5. The file sink is size-capped by rotation (64 MiB x 8 files = 512 MiB) so logs cannot fill the disk by themselves. Stdout under a supervisor is unbounded by us: the unit must set a journald rate limit (documented, not enforced here).

| Subsystem | Sustained | Burst |
|---|---|---|
| consensus | 8 KiB/s | 32 KiB |
| exec / import | 8 KiB/s | 32 KiB |
| net (peers, protocol) | 4 KiB/s | 16 KiB |
| sync | 4 KiB/s | 16 KiB |
| rpc | 4 KiB/s | 16 KiB |
| db / storage | 2 KiB/s | 8 KiB |
| other | 2 KiB/s | 8 KiB |
| ERROR and alarm lane | 4 KiB/s | 16 KiB |
| **Global cap** | **32 KiB/s** (2.7 GB/day worst case) | 128 KiB |

At about 200 bytes per line the global cap is about 160 lines/s. The sum of subsystems (32 KiB/s plus the 4 KiB/s lane) is above the global cap on purpose: the global bucket is the hard bound, the others only stop one subsystem from taking all of it. Pass: under a flood of per-message events the bytes written to the sink stay at or under the global cap times elapsed time plus the burst.

## 7. Disk watermark

Measured on the filesystem that holds the datadir (`chain.mdbx`, `static/`, `safety/`). Both a percent of capacity and an absolute floor; the larger bound wins, so a small disk is protected by the percent and a big disk is not made to waste space. Proposals:

| Threshold | Free space [D] | Action |
|---|---|---|
| X: stop import | below the larger of 10% and 20 GiB | `StopImportAlarm`: finish the block being committed, stop taking new blocks, raise one alarm `xone-node: disk_low free=<bytes> stop_below=<bytes>` |
| Resume | above the larger of 15% and 30 GiB | `Continue`. Hysteresis band of 5 points so a growing log or a pruner does not flap it. |
| Y: exit | below the larger of 2% and 4 GiB | `Exit`: clean shutdown, non-zero status |

Rules:

- `DiskWatermark::decide(free, prev)` is pure. The caller samples free space every 5 s and alarms only on a transition into `StopImportAlarm`. It acts on `Exit` after two consecutive samples below Y, a bounded wait of about 10 s, so a momentary reading (a snapshot being deleted, a quota update) does not end the process. The caller measures free bytes with `statvfs`, which needs a dependency or `unsafe` that this crate forbids; it belongs to the wiring item (not done).
- While stopped, RPC reads, serving and consensus timers continue. Pruning is allowed because it frees space (ADR-005 section 10 unlinks whole chunk files). A validator that cannot import cannot vote on blocks it lacks, so StopImport is in effect a "not producing" alarm; the operator must act.
- Y must cover the largest single write set: one block commit, one MDBX growth step, one chunk file, the safety-record slot (ADR-010, 1 KiB, fsynced) and the journal. The numbers above are not derived from those sizes; they are proposals until the sizes are measured (question 3).

**Interaction with ADR-005 section 9.** Two lines of defence for the same disk:

1. The watermark acts first, on a trend, before any write fails. The state is consistent because the stop happens at a block boundary.
2. ADR-005 `Degraded(NoSpace)` is the second line, for a disk filled faster than the sample period (another process, a log burst): abort the transaction, truncate to committed `len`, `may_sign()` false, one alarm `xone-db: degraded cause=no_space`.

The ENOSPC path does not exit; the watermark `Exit` does. A failed `fsync` is still fail-stop (ADR-005) and is not changed by this ADR. **Proposal for ADR-005's open "reserve" value:** use the resume threshold as the free-space level above which `NoSpace` retries, so the two mechanisms agree and do not alternate. One alarm per cause: the watermark alarm and the `no_space` alarm are different lines and each fires once per transition.

## 8. Safe defaults

"Config" means an operator can change it; every value is a proposal. Limits are config with safe defaults; there is no switch that turns a limit off (plan G7: no validation or safety bypass), though a bound can be raised.

| Setting | Default [D] | Config |
|---|---|---|
| Per-peer Q1 / Q3 / Q5 / Q7 bounds | 256 / 16 / 128 / 8 | no (code constants) |
| Global Q2 / Q4 / Q6 | 4,096 / 16 / 4,096 | no |
| Sync Q8 / Q9 / Q10 / Q11 | 2,048 / 128 / 64 / 2 | Q8 to Q10 yes (memory) |
| Max peers | 50 | yes |
| RPC connections HTTP / WS / IPC | 256 / 128 / 16 | yes |
| RPC in flight global / per connection | 64 / 8 | yes |
| RPC class concurrency C1 / C2 / C3 | 32 / 8 / 2 | yes |
| RPC bucket (units/s, burst) | 200 / 400 | yes |
| RPC deadline C0 / C1,C2 / C3 | 2 s / 10 s / 30 s | yes |
| `getLogs` range, logs, response | 5,000 / 10,000 / 20 MiB | yes |
| Trace methods on public listener | off | yes |
| Request body, batch items | 5 MiB, 100 | yes |
| Snapshot sessions, rate | 4, 16 MiB/s | yes |
| Peer thresholds, decay, ban | 100, 10/min, 30 min | yes |
| Message size caps | section 5 | no |
| Log subsystem buckets, global cap | section 6 | yes |
| Log file rotation | 64 MiB x 8 | yes |
| Watchdog warn / alarm / fail-stop | 100 ms / 2 s / 10 s | warn and alarm yes; fail-stop yes but not below 2x the alarm |
| Disk stop / resume / exit | 10% or 20 GiB, 15% or 30 GiB, 2% or 4 GiB | yes, with `exit < stop < resume` enforced at start |
| P0 / P1 thread counts | 1 + 2 verify / 1 + cores-based | no for the `consensus` and `exec` threads |

Rollback (issue): every limit is config with a safe default; a bad value is fixed by config and restart.

## 9. Owner questions

1. Confirm the P0 budgets: round timer p99 lag at or under 50 ms, vote path p99 at or under 100 ms, flood p99 at most 1.25 times idle. `docs/perf.md` has no vote-path line.
2. Confirm the watchdog levels (100 ms, 2 s, 10 s) and that 10 s fail-stop is acceptable for a node that holds a validator key.
3. Disk: are 10%/20 GiB, 15%/30 GiB and 2%/4 GiB right for the target hosts? Needs the daily growth and the largest commit write set (Gate B, XA-06).
4. Should the ADR-005 NoSpace "reserve" equal the resume threshold?
5. Trusted and static peers: exempt from ban? Which peers count as trusted?
6. Is a public RPC listener in scope for the default build, or are traces off and C3 absent there?
7. Log sink for production: file with rotation, or stdout to journald with a unit-level limit?
8. May `consensus` and `exec` be pinned to CPUs (`AllowedCPUs`) in the reference unit file?

## 10. Not decided here

Tx pool size and eviction; peer discovery limits; the Engine-style external API (none planned); memory limits of the EVM and the trie cache; global memory budget (cgroup `memory.max` guidance); alert routing and the metric names beyond the few named above.
