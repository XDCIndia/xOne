---
name: xone-rs-durability-signer-gate
description: The durability and signer-safety gate of the xOne Rust rebuild. Crash tests at every block-commit, segment-publication and safety-record boundary, lost, short and reordered write injection, stale-backup restore, signer handoff and failover, and a monotonic anti-equivocation record. Use when a change touches xone-db, static files, commit or recovery code, the safety record, the signer, SetHead, unwind, snapshot import or backup restore, or when the task says crash test, power loss, durability, cross-store commit, generation marker, double sign, equivocation, failover, handoff, "safety record".
---

# xone-rs-durability-signer-gate

A database transaction is not a transaction across every file. Hot state sits in MDBX; headers, bodies and receipts sit in static files. One MDBX commit cannot by itself publish those files. Chain rollback and signing history also have different lifecycles: chain state may go back, signing safety may never. This gate must pass before any public producer test (Gate D before Gate E).

Live lessons: xOne's `SetHead` deleted the canonical-index entry and then failed; reth-xdc left a sign-tx sender nonce partly reverted after an unwind; xOneGo lost sibling-block storage and drifted its canonical index.

## Operating rules

1. Run on datadirs made for the test, on a local devnet or test host. Never on a public producer or a datadir another run needs.
2. A wrong root, a head ahead of its data, or a lowered safety record after any fault is a P0. A passing run has no manual repair step.
3. `kill -9` is a process crash. It does not model power loss. Host and device faults are injected under a declared filesystem and device model (state it in the report).
4. Random kill counts supplement targeted boundary coverage. They never replace it.
5. Fault injection code is a test-build feature and is absent from release artifacts (checked by the release-tree check in `xone-rs-release`).
6. A failed fsync is fatal for the affected store: stop, alarm, do not retry silently, do not sign.

## The contract under test (proposal until accepted in an ADR)

- **Ordered staging.** Segment data is written under staging names with checksums, flushed, directory-synced. Incomplete files are invisible to readers.
- **Generation marker.** The MDBX commit that moves the head also records a generation number and the segment manifest (lengths, checksums) for that generation, under the declared sync mode.
- **Publication.** After the commit, staged data is made visible (rename or visible-length update), idempotently.
- **Restart reconciliation.** On start, compare the MDBX generation with the manifest and the files. Truncate or quarantine anything beyond the committed manifest; roll forward only when checksums verify; otherwise refuse to start with a typed error. Never serve a head beyond verified data.
- **Pruning** is interruptible and resumable, with the same visibility rule.
- **ENOSPC / EIO.** Fail-stop with one alarm line, no corrupted datadir, resume after space is freed.
- **Signer.** The precise signing intent (type, round, digest) is persisted, flushed, and acknowledged by the driver before a signature is released; the network effect follows only after that. Retry after an ambiguous send is idempotent: same intent, same signature or no signature, never a conflicting one.
- **Separate lifecycle.** The safety store is separate from chain state. `SetHead`, changeset unwind, backup restore and snapshot import never lower it. Monotonic: highest voted round and per-type highest signed round never decrease.
- **Single-signer boundary.** One authenticated signer per key. A local process check does not fence a duplicate on another host; the handoff uses a fence that works across hosts, or a conservative wait that exceeds the old signer's possible rounds.
- **Stateful signatures** (only if a stateful PQ scheme is ever studied, see `xone-rs-pq-readiness`): one-time state is reserved durably before use and is never restored by a datadir rollback.

## Steps

1. **Boundary list.** Enumerate every boundary: each journal write, the MDBX commit, each segment flush, the manifest update, publication, head publication, each safety-record write, each signature release, each prune step. The list is part of the report.
2. **Targeted crash test.** For each boundary, kill the process there (hook or failpoint). Restart. Check: same canonical state as the contract predicts, no visible partial file, safety record at or above every released signature, `xone-rs-diff-exec` sequential shows 0 mismatches for the next 200 blocks.
3. **Device-fault test.** Under the declared model, inject lost, short and reordered writes and sync failures at the same boundaries. Expected result is recovery or a typed refusal to start. Silent acceptance of damaged data fails the gate.
4. **Rollback tests.** Reorg across special transactions (`0x89`, `0x90`), `SetHead`, unwind and re-execute. Safety record unchanged or higher; canonical index consistent.
5. **Stale-backup restore.** Restore an old datadir copy. The node either keeps the newer safety record or refuses to sign until its safety state is proven at least as high as every signature that left any host.
6. **Handoff and failover.** Move the signer between hosts with the old one alive, dead, and partitioned. No two signatures for one round on different digests. Retry pending votes after an ambiguous send.
7. **Deterministic simulation.** Control clock, network, randomness and disk effects around the real consensus and storage code. Save the seed, the minimized trace and the expected invariant failure. Fixed seeds in CI, new seeds nightly. Invariants: no conflicting commit, no conflicting signature, no head ahead of data.
8. **Resource faults.** Disk full, I/O error, queue overflow, cancellation: each has a defined transition and no signing in a degraded state.

## Gates

- Every boundary in step 1 has a crash test result; 0 failures.
- Device-fault runs: 0 silent acceptance; each failure is a recovery or a typed refusal.
- 0 conflicting signatures across rollback, kill, stale restore, retry and handoff, in the targeted cases and in the simulator (seed count stated).
- 0 safety-record decreases.
- Seeds, traces, filesystem model and commit are recorded.

## Done

A report on the work item: boundary list, results per boundary, fault model, simulator seeds, commit, host. Any failure has a divergence record and goes to `xone-rs-conformance-loop`. Three unchanged failures escalate with a written root-cause decision.
