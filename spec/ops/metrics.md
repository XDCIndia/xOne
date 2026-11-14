# Prometheus metrics and health endpoints

Rust: `xone_node::observe::metrics` (`Snapshot`, `encode`, `MetricsServer`). The node fills a `Snapshot` from its own counters (`MetricsSource`); the module formats it as text format 0.0.4 by hand (no `prometheus-client`: about 80 lines, no registry, no global). The test `exposition_parses_and_lists_the_named_metrics` runs a minimal format checker (valid names, `# TYPE` before samples, counters end in `_total`, quoted labels, numeric values).

| metric | type | meaning |
|---|---|---|
| `xone_build_info{version}` | gauge | the build stamp, value 1 (same string as `web3_clientVersion` and ethstats `node`) |
| `xone_head_block_number` | gauge | head number (absent without a head) |
| `xone_head_block_hash_info{hash}` | gauge | head hash as a label, value 1 |
| `xone_head_block_age_seconds` | gauge | now minus head timestamp |
| `xone_syncing`, `xone_peers` | gauge | |
| `xone_txpool_pending`, `xone_txpool_queued` | gauge | |
| `xone_blocks_imported_total` | counter | blocks this process imported onto the head (`Core::imported`); starts at 0 on each start, never decreases on reorg or set-head, excludes failed imports, side blocks and start-up replay; `rate()` is the import rate. Chain height is `xone_head_block_number` |
| `xone_rpc_in_flight{class}` | gauge | running requests per ADR-007 section 4 class |
| `xone_queue_depth{queue}`, `xone_queue_overflow_total{queue}` | gauge, counter | ADR-007 queue ids `Q1`..`Q18` |
| `xone_watchdog_lag_seconds` | gauge | lateness of the watchdog tick |
| `xone_consensus_round`, `xone_highest_qc_round` | gauge | |
| `xone_timeouts_total`, `xone_missed_eligible_turns_total` | counter | the miss counter increments in `scrape_missed_turn_and_health_endpoints` |
| `xone_storage_bytes` | gauge | datadir size |
| `xone_log_suppressed_lines_total`, `xone_log_dropped_lines_total` | counter | from `Logs::counters` |

**Endpoints** on `--metrics.addr` and `--metrics.port` (default 127.0.0.1:6060): `/metrics`; `/livez` always 200 while the process answers (a partition never makes it fail, so it cannot cause restart loops); `/readyz` 200 only when `Snapshot::ready(max_head_age)`: a head exists, the head is not older than the limit, not syncing, not `degraded` (recovery, durability or signer-fencing fault), not shutting down; otherwise 503. A single-node devnet that cannot meet the age rule passes a larger `max_head_age_secs`; the rule is never switched off. The geth reference is `node/health.go` (XDC addition, pin `6fc8e8c2`); xOneGo `health_endpoints.go:44-111` is the lesson.

**Bounds.** One thread, one request at a time, 2 s read and write timeouts, a 2 KiB request-line read, `GET` only.
