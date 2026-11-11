# Overload test plan (XA-11, ADR-007, Gate C)

Status: plan. Only the unit tests of the primitives in `crates/xone-node/` exist (queue overflow policies, token bucket bytes/s under a flood with a fake clock, watermark hysteresis, peer ban). No flood test has run. Rules: throwaway keys and local devnets only; never a live validator, key or host; no expected value edited by hand; any fault hook behind the `fault-injection` feature and absent from release artifacts.

Method (from `docs/perf.md` section 4): open-loop load, latency measured from the intended send time, idle run first, flood at 50 / 90 / 120 percent of the measured knee, three runs per setup, pinned fields recorded. Above saturation the pass criterion is bounded queues and recovery.

## O1. RPC flood versus the round timer and vote path

- Load: `bench/workloads/rpc-sync-interference.yaml` extended with C2 (`eth_getLogs` at the range cap) and C3 (`debug_traceTransaction`) mixes, plus 10x the knee of C1 reads, on all three transports.
- Measure: `round_timer_lag_ms` and vote-path latency (receive to send) p50, p99, p99.9 idle versus flood; import latency; queue depth max per queue; RPC 429 count; MDBX reader age.
- Pass: under flood, timer lag p99 <= 50 ms and vote path p99 <= 100 ms and <= 1.25 x idle (proposal, ADR-007 section 3); every queue at or under its bound; RPC rejects with `-32005` instead of queueing; recovery to idle p99 within 30 s after the flood stops.
- Variants: client disconnects mid-trace (the worker stops within one transaction and the reader is released); slow WS subscriber (dropped at 1,024 messages).
- Needs that does not exist: the node with P0 and P1 threads; the RPC server with admission, cost classes and cancellation; the EVM step hook for the cancel flag; the round timer and vote-path metrics; a local multi-validator devnet (`sim/` is a pure simulator and has no sockets); an open-loop load generator (named in `bench/README.md`, not built); the XA-08 idle baseline of vote-path latency.

## O2. Log flood bytes per second

- Load: a generator that drives each subsystem's per-message path at 100,000 events/s for 5 minutes, at DEBUG enabled and at default level.
- Measure: bytes written to the sink per second and in total; suppressed-line summaries; time to write an ERROR line during the flood.
- Pass: default level writes no INFO line from a per-message path (grep of the output); total bytes <= 32 KiB/s x elapsed + 128 KiB burst; an ERROR written during the flood appears within 1 s; at most one summary line per subsystem per 10 s.
- Done already: `TokenBucket` bound under a fake clock (unit test).
- Needs: the log layer with per-subsystem buckets and the reserved alarm lane; the writer thread and Q17; a CI check that no per-message site logs at INFO (a lint or grep rule).

## O3. Disk full with ADR-005

- Setup: a loop-device or small tmpfs filesystem as the datadir (same harness as `tests/fault/PLAN.md` loop-device gate; needs root or an approved runner).
- Steps: import a replay range while a ballast file shrinks free space past each threshold in turn; case A slow fill (watermark acts first); case B fast fill (ballast written between two samples, so `NoSpace` fires); case C fill during a commit; case D free space after stop (resume with hysteresis, no flapping); case E fill below Y (clean exit).
- Pass: A: one `disk_low` alarm line, import stops at a block boundary, RPC reads and pruning continue, resume only above the resume level, then import continues. B: `Degraded(NoSpace)` with one `no_space` alarm, no torn commit. C and E: after restart `reconcile` reports a consistent head and the safety record file is byte-identical (ADR-005, ADR-010). No corruption: full `xone db verify` after every case.
- Needs: the `xone-db` implementation (only traits and `reconcile` exist); `statvfs` free-space sampling (a dependency or an approved `unsafe`-free crate, since this crate forbids `unsafe`); the import pipeline and its stop-at-block-boundary hook; the pruner; `xone db verify` (ADR-005 Gate B tool).

## O4. Malformed-peer flood

- Load: raw RLPx peers (the S2 spike client in `spikes/net` is the start) sending (a) undecodable frames, (b) a frame header above the cap, (c) valid-looking votes with bad signatures, (d) 1,000 peers each sending below its own ban threshold, (e) a peer that stops reading.
- Measure: time to disconnect, memory and CPU of the node, queue depths, `bench/workloads/malformed-traffic.yaml` rejection cost, round timer lag while it runs.
- Pass: (a) the peer is banned after 10 messages; (b) banned at once with no body allocation (heap sample); (c) banned after 4 and verify CPU stays in the P0/P1 pools; (d) memory at or under the arithmetic bound of ADR-007 section 2 and the global queues hold; (e) the socket write buffer is bounded and the peer is dropped. In all cases the vote-path budget of O1 holds.
- Needs: `xone-net` (decoder with frame-length check before read, peer manager, ban table); `PeerBudget` wired to every decoder error; a raw-peer flooder; the global inbound connection limit per /24.

## O5. Sync and snapshot-serving flood (not named in the issue, same risk)

- Load: 50 peers requesting headers, bodies and snap ranges at full speed while the node imports.
- Pass: Q7 disconnects past 8 outstanding requests; snapshot serving stays at or under 4 sessions and 16 MiB/s; import p99 within 1.25 x idle; the O1 P0 budget holds.
- Needs: serving code for eth and snap/1; Q15.

## Exit

All of O1 to O5 pass three times in a row with the P0 budget met each time. Not yet decided: which of these run in CI (O2 and the watermark cases of O3 on tmpfs are the cheapest) and which only on the assigned bench host.
