# Signer safety test plan (XA-07, ADR-010)

Status: plan for Gate B and Gate D. Only the pure rules and the simulator exist (`crates/xone-consensus/src/v2/safety.rs`, `sim/`). Rules: throwaway keys and local simulated validators only; never a real validator key or host; no expected value edited by hand; fault code only under the `fault-injection` feature and absent from release artifacts.

## 1. Simulator gate (exists)

- `cargo test --locked --release -p xone-sim`: 20,000 seeds, 0 violations, both mutants caught. Add this as a CI step (not done here: `.github/` is outside the allowed paths of the item). `cargo test --locked` (debug) runs 1,000 seeds as a smoke test.
- A violation: save the seed, print the event trace, fix, rerun all seeds, twice in a row (issue loop). A trace minimiser is not written.

## 2. Gate B: real store

The file store of ADR-010 section 4 (two 1024-byte slots, `fsync` per write). Same fault model F1-F8 as `tests/fault/PLAN.md`, same harness.

| ID | Kill or fault | Expected |
|---|---|---|
| SK01 | before the slot write | old record |
| SK02 | slot written, unsynced (F2, F5) | old or new record; nothing signed in that call |
| SK03 | torn slot at 512-byte cut (F3) | the other slot; the torn one fails its checksum |
| SK04 | both slots torn or damaged | `Corrupt`, refuse; the record is never reset |
| SK05 | fsync error (F6) | process exits, `FSYNC_FAILED` written, same-boot restart refuses |
| SK06 | ENOSPC at `init` (F7) | `init` fails, no partial file counts as a store |
| SK07 | synced, ack lost | new record; retry signs the same bytes |
| SK08 | missing file with an existing datadir | refuse; only `init` creates one |
| SK09 | record of another identity | refuse |
| SK10 | `load()` after a crash with a page-cache-only frame | the frame is synced before it is returned (check with the loop-device gate) |
| SK11 | second process on the directory | refused by `flock` |

Chain-side operations must leave `datadir/safety/` byte-identical: `unwind_to`, `SetHead`, restore of `chain.mdbx` and `static/`, snapshot import (hash of the directory before and after). The `xone-rs-restart-unwind-gate` kill test inside the vote path uses these kill points.

Differential check of the in-memory store against the file store: same intent streams, same loaded record after every kill point.

## 3. Gate D: multi-host fencing

Devnet with throwaway keys. Hosts A and B hold the same key. Each case runs with `W` shortened by config on the devnet only (the production formula is not changed).

| ID | Case | Expected |
|---|---|---|
| MH1 | A signs; B starts | B sees A's signatures, never opens |
| MH2 | A stopped; B running its window | B opens `W` after its last sighting; its record is above A's last signature |
| MH3 | B started at the same time as A | record the outcome; both may open (R2). Pass only if the random start delay or an operator rule is accepted by the owner |
| MH4 | A restored from an old copy of `datadir/safety/` | A raises its record from sightings, then opens; no conflicting signature on the devnet |
| MH5 | restore of the whole disk image | same as MH4 |
| MH6 | restore while partitioned | A stays closed until gossip returns |
| MH7 | a signature sent only into a partition, then restore | document the conflict (R1); expected to fail the no-conflict assertion, kept as a known-limit test |
| MH8 | clock jump (wall clock) | no effect; the window uses a monotonic clock |
| MH9 | storage degraded (ADR-005) on the signer | no signature of any kind; resumes only after restart and reconcile |

## 4. Layering fixture

- `tests/signer/check-layering.sh`: runs `cargo metadata`, builds the dependency graph, and fails if any workspace crate other than the safety-unit crate depends on `xone-keys` (direct, dev, build or through a re-export), or if `xone-sim` or the `fault-injection` feature appears in the graph of `bin/xone`.
- Fixture `tests/signer/fixtures/bypass/` (not a workspace member): a crate that depends on `xone-keys` and calls `sign` directly. CI runs the script on the fixture and requires a non-zero exit. A check that passes on the fixture fails CI.
- Compile-time fixture: a second crate that builds an `Authorized` without an `Ack` must fail to compile (private constructor). CI runs `cargo check` on it and requires failure.
- Both fixtures wait for `xone-keys` (later item). Until then the script covers the `xone-sim` and `fault-injection` rule only.
