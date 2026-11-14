# Pass 5 module and ownership map

Source pin: `54d8ae7da15c87dd77a7a86ca695c813bad5ec91`. Focus: persistence, disk admission, execution timing and the newly selected Seismic Reth client.

| xOneRS surface | Current evidence | Finding or existing owner |
| --- | --- | --- |
| Storage schema and row API | `crates/xone-db/src/mdbx.rs` (24 table variants, range/prefix helpers) | #326/#329, #135/#149/#176 |
| Append-only history | `crates/xone-db/src/segments.rs` (writer cache, frames, scan/quarantine) | #327/#328/#332; #194 compression |
| Canonical commit/prune/unwind | `crates/xone-sync/src/mdbx/mod.rs` | #326/#329/#331; #11/#177 |
| Disk-backed state and trie rows | `crates/xone-sync/src/mdbx/diskflat.rs`, `trierows.rs`, `snapstate.rs` | #145/#149/#259 |
| Disk admission | `crates/xone-node/src/diskguard.rs`, `services/mod.rs`, `services/snapsync.rs`, `services/producer.rs` | #330 |
| Block lifecycle and clean close | `crates/xone-node/src/services/core.rs` | `Drop` already flushes; #226 phase telemetry and #247 pipeline |
| Receipts and log reads | `crates/xone-sync/src/mdbx/codec.rs`, `crates/xone-rpc/src/eth.rs`, `json.rs` | #332/#333; #258/#276 |
| Metrics and measurement | `crates/xone-node/src/observe/metrics.rs`, `services/stats.rs`, `docs/perf.md`, `bench/` | #13/#41/#149/#226 |

Seismic Reth is a Rust workspace (`Cargo.toml`) based on Reth, with MDBX/static-file storage, the `crates/prune` worker, `crates/stages` pipeline, `crates/rpc` APIs/auth, node and transaction-pool services, and database metrics. Concrete paths and present/partial/missing comparisons are in `feature-matrix.md`.

Seismic-specific modules reviewed: `crates/seismic/txpool/README.md` (registry fee rules and maintenance), `crates/seismic/rpc/README.md` (RPC semantics and verification scope), `crates/seismic/node/src/node.rs` (local pool backup), `crates/rpc/rpc-builder/src/body_auth.rs` (alternate authenticated HTTP server), `crates/engine/tree/src/backup.rs` (backup service), and `docs/seismic/features.md` (shielded protocol). These are under [the pinned tree](https://github.com/SeismicSystems/seismic-reth/tree/faa3a30c430676dfc2d0224fe2bcce18bf0e1e88). Protocol differences are recorded as skips; portable mechanisms map to existing issues or the eight new issues.

Admin/client web UI, invoicing, tax, refunds, multi-currency billing and GDPR account export from the generic audit template are not capabilities of an execution-node product. They remain out of category, not missing features. Node APIs, exporters, hooks, configuration, metrics, import/migration, retention and recovery are the applicable surfaces.
