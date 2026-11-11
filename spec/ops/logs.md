# Logs: levels, budgets and rotation

Rust: `xone_node::observe::logs` (`Logs` service, `LogLayer`, `Limiter`, `Rotating`). Source of the numbers: ADR-007 section 6 and Q17.

* **Levels.** `--verbosity n` (0 silent, 1 error, 2 warn, 3 info, 4 debug, 5 trace) and per-module overrides by target prefix (`xone_net` matches `xone_net::peer`, not `xone_network`; longest prefix wins). Checked in `Layer::enabled`, so a disabled event costs no formatting.
* **Budgets.** Subsystem from the target's crate: consensus (`xone_consensus`, `xone_signer`) and exec (`xone_executor`, `xone_evm`, `xone_state`, `xone_builder`) 8 KiB/s burst 32 KiB; net, sync, rpc 4 KiB/s burst 16 KiB; db and other 2 KiB/s burst 8 KiB; global 32 KiB/s burst 128 KiB. A line needs its subsystem and the global bucket. ERROR lines use only the reserved lane (4 KiB/s, burst 16 KiB), so the bound on bytes written is global cap times elapsed plus burst, plus the lane. `flood_is_bounded_by_the_global_cap` offers a 200-byte line every millisecond over every subsystem for one simulated hour and asserts both the upper bound and that the limiter is not starving.
* **Suppression** is counted; at most one `suppressed lines=N bytes=M subsystem=S` line per subsystem per 10 s, emitted with the next passing line (no timer), drawn from the lane, and the owed ones are written at `stop`.
* **Queue.** Q17: 8,192 lines to one writer thread, `try_send`, newest dropped and counted; hot threads never block on logging (one short mutex section for the buckets).
* **Sinks.** `SinkCfg::File { path, max_bytes, files }` rotates by size: `path`, then `path.1` (newest) to `path.{files-1}`; the default is 64 MiB x 8 (512 MiB). `SinkCfg::Stdout` for a supervisor: the unit must set a journald rate limit (ADR-007 section 6 rule 5), this module cannot enforce it.
* **Rule 1, no INFO on a per-message path.** `no_info_on_per_message_paths` scans `xone-net`, `xone-sync`, the pool (`xone-builder/src/txpool`) and `xone-rpc` sources for `info!(` and fails on any (go-ethereum#1953); it asserts it found the files so it cannot pass vacuously.
* **Global state.** `Logs::start` installs the tracing subscriber with `set_global_default`, once; a second install returns an error. Nothing else here is global.

**Not covered.** The line format is `<unix ms> LEVEL target message k=v` (no JSON); the config keys for file path, size and count are not in `Config` yet, only in `SinkCfg`.
