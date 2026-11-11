# Baseline procedure

Contract: `docs/perf.md`. Workloads: `bench/workloads/`. This is a procedure. No numbers exist yet; none are claimed.

## Scope and limits

- The owner assigns one dedicated host. Do not start before that. Do not use a fleet host, a producer host or a validator.
- Both clients run as read-only followers: no validator key on the host, no mining, no signing. Peers are trusted static Apothem peers, same list for both clients, same peer count.
- Clients run one at a time on the same host, same datadir device and filesystem, same dataset height and hash, same prune/retention mode, same durability class. If a client cannot be given the same retention or durability, say so in the record and do not compare the two as equals.
- Never compare RSS with cgroup memory, full state with pruned state, or a tip-following run with an active-import run. Run import workloads and tip-follow workloads separately.

## Steps (per client, per workload)

1. Pin the host: SMT and governor fixed (record them), no other load, swap off. Record NUMA placement; bind the process and disk to one node.
2. Build or fetch the binary. Record SHA-256 and the version stamp (`xone --version`; geth-XDC and xOneGo version output).
3. Prepare the datadir at the dataset start height and hash from the workload spec. Record the start hash.
4. Put the client in a cgroup with a fixed `memory.max` and `cpu.max`, same for both clients.
5. Capture the environment: `bench/collect/env.sh --datadir DIR --binary PATH --pid PID > env.json`.
6. Find the knee for rate workloads with a ramp run; record it in the workload copy for this host.
7. Run each workload: cold run first (restart, cache dropped), then warm. At below, near and above saturation for open-loop workloads. Three runs each from the same declared state.
8. Write per-block latencies as CSV with a `latency_ms` column; summarise: `python3 -I bench/collect/summarise.py lat.csv`.
9. Sample RSS (`VmRSS`, `RssAnon`, `RssFile`), cgroup `memory.current` and `memory.stat`, and page cache separately every 1 s; sample `/proc/diskstats` and network counters for I/O amplification and bytes.
10. Report the three runs and the spread against the workload tolerance. If outside tolerance, find the noise source, fix the setup, rerun. Record the load generator's own limit.

## Must not

- No validators, producer hosts, keys or fleet hosts.
- No closed-loop-only load; no mixing durability modes; no edits to expected vectors or `input_hash`.
- No illustrative charts: figures come only from measured raw histograms.

## Run record template

```yaml
run_id: ""
date_utc: ""
operator: ""
host: {tier: "", env_json: "env.json"}      # output of collect/env.sh
client: {name: "", version_stamp: "", binary_sha256: "", build_flags: ""}
chain: {network: apothem, start: {height: 0, hash: ""}, end: {height: 0, hash: ""}, fork_table_pin: ""}
node_mode: {role: follower, sync_mode: "", prune_mode: "", peers: 0, peer_list: ""}
durability: {mdbx_sync_mode: "per ADR-005", other_flags: ""}
workload: {id: "", spec_path: "", spec_git_sha: "", input_hash: "", resolved_ranges: []}
state: {cold_or_warm: "", restart_before: true}
load: {model: "", level: "below|near|above", rate: 0, generator: {name: "", version: "", sha: ""}}
repeat: {index: 1, of: 3}
latency_ms: {n: 0, p50: 0, p95: 0, p99: 0, p99.9: 0, max: 0}   # from summarise.py; stage breakdown alongside
throughput: {}
errors: {}
queue_depth: {max: 0, p99: 0}
cpu: {}
memory: {rss_kb: 0, cgroup_memory_bytes: 0, page_cache_bytes: 0}   # three separate numbers
io: {device_bytes_written: 0, logical_bytes_committed: 0, write_amplification: 0}
network_bytes: {in: 0, out: 0}
spread: {p50_pct: 0, p99_pct: 0, within_tolerance: false}
notes: ""
```
