# Fault-injection plan for ADR-005 (XA-06)

Status: implemented for gates G1 to G4 (Gate B, XA-06): `tests/fault/tests/{kill,rows,store,import,process_kill}.rs`; G5 to G8 (loop device, device-mapper, two file systems) remain. Protocol, kill point IDs and decision rows come from `docs/adr/ADR-005-storage-and-publication.md` (called "the ADR" below). Fault IDs F1-F8 are the ADR section 3 model.

Rules: run only on test datadirs, never on a producer host, keys or live validators. Fault code is a test-build feature (`fault-injection`) and must be absent from release artifacts; the release-tree check from `xone-rs-release` proves it. No expected result is edited by hand to make a run pass.

## 1. Kill points

A kill point is a named failpoint compiled under `fault-injection`. `fail_at(K)` aborts the process (F1) or, in the in-memory harness, snapshots the simulated disk (F2).

| Group | IDs | Boundary |
|---|---|---|
| Journal | K01 before journal write; K02 written, unsynced; K03 fsynced, not renamed; K04 renamed, dir not synced | S1 |
| Execute | K05 mid-execution | S2 |
| Stage | K06 headers frame written; K07 bodies; K08 receipts; K09 new chunk file created, dir not synced | S3 |
| Segment sync | K10 headers fsynced; K11 bodies; K12 receipts; K13 directories fsynced | S4 |
| MDBX txn | K14 transaction complete, before commit | S5 |
| Commit | K15 error inside commit (loop-device gate only); K16 commit returned | S6 |
| Publish | K17 after visible-length swap; K18 journal unlinked, dir not synced; K19 dir synced | S7 |
| Unwind | U1a journal written; U1b renamed; U3a before commit; U3b commit returned; U4a after first truncate; U4b after all truncates; U5a journal deleted; U5b dir synced | section 8 |
| Prune | PR1 before floor commit; PR2 after commit; PR3 after first unlink; PR4 after last unlink; PR5 dir synced | section 10 |
| Start-up repair | RP1 after truncate, before file fsync; RP2 after file fsync, before dir fsync; RP3 after quarantine copy, before truncate | section 7 |

Coverage statement: the list is the full boundary list required by `xone-rs-durability-signer-gate` step 1. A boundary added to the ADR without a new ID here fails review. Random kills (section 6) supplement this list and never replace it.

Expected result at every ID, after restart: the head is one committed generation `g` (the last commit that returned) or `g-1` (kill at or before K15), `reconcile` returns the row the ADR table predicts, `xone-rs-diff-exec` sequential replay shows 0 mismatches for the next 200 blocks, and no reader saw a frame beyond the manifest.

| Kill at | Expected head | Expected `reconcile` row |
|---|---|---|
| K01-K05 | g | D11 (segments untouched) |
| K06-K14 | g | D11, D12 or D13 (staged tail, depending on what reached disk); an orphan new chunk file (K09, K13) is deleted by start-up, not by a table row |
| K15 | g or g+1 (unknown commit outcome; the manifest decides) | D13 if g, D11 if g+1 |
| K16-K19 | g+1 | D11 |
| U1a-U3a | g | D11 |
| U3b-U4b | g+1 (unwound) | D13 (segments longer than manifest) |
| U5a-U5b | g+1 | D11 |
| PR1 | g | D11 |
| PR2-PR5 | g+1 | D11, then pruned-file cleanup |
| RP1-RP3 | unchanged | same row again (repair is idempotent) |

## 2. Fault-injecting file abstraction (no device-mapper)

The segment writer takes its I/O through two small traits defined in `xone-db` (internal, not public API; added when the writer is implemented):

- `SegFile`: `write_at`, `read_at`, `len`, `set_len`, `sync_all`.
- `SegDir`: `create`, `rename`, `remove`, `sync`.

Two implementations: `OsFile`/`OsDir` (production, `std::fs`) and `FaultFile`/`FaultDir` (test only, in-memory).

The in-memory model keeps two layers per file: `durable` (what survived the last `sync_all`) and `pending` (ordered list of unsynced writes, truncations, creates and renames). A simulated crash builds the post-crash disk as `durable` plus a chosen subset of `pending`:

| Fault | How it is generated |
|---|---|
| F2 power loss, fsync honoured | Drop all `pending` for files and directories without a later `sync` |
| F3 torn or short write | Keep a prefix of one pending write, cut at a seeded offset on a 512-byte grid (also run on a 4096-byte grid) |
| F4 reordered writes | Keep any seeded subset of `pending` writes, in any order, between two sync barriers |
| F5 delayed flush | Keep a seeded prefix of `pending` that ends before the last write, or none |
| F6 fsync error | `sync_all` returns `EIO` or `ENOSPC` once at the n-th call; pending data of that file becomes "unknown": the crash image is any subset (and the page-cache view after the error keeps all pending, to test the same-boot guard P2) |
| F7 ENOSPC | `write_at`, `set_len` or `create` returns `ENOSPC` once at the n-th call, or from the n-th call on (quota set by the test) |
| F8 EIO | `write_at` or `read_at` returns `EIO` at the n-th call |

Enumeration, per kill point K and per fault: first all exhaustive cases (empty subset, full subset, each single pending item alone, each single pending item missing), then `N_SEEDS` seeded random cases. Seeds are saved with the failing trace and minimized by removing pending items one at a time until the failure stops. The saved record is `(seed, kill point, fault, trace)`.

The test oracle for each case is the ADR table: run the real scanner and `reconcile` on the crash image, apply the `Recovery`, reopen, and check:

1. `Recovery` is a `Repair` or `Ready` and the reopened head equals one committed generation; or it is a `Refuse` with a typed reason. Nothing else passes.
2. No byte beyond the manifest `len` is returned by any read.
3. Every block up to the head reads back with a valid CRC and the tail digest matches.
4. `Durability` is `Degraded` for every failure event in the ADR section 9 list and `may_sign()` is false.

Silent acceptance of damaged data (a head served whose frame fails CRC or digest) fails the gate.

What this layer cannot cover: MDBX internals. MDBX writes through its own file descriptors (libmdbx C code), so `FaultFile` does not see them. MDBX is covered by the kill points before and after its commit (K14, K16) plus the loop-device gate in section 4. The ADR relies on `Durable` mode for MDBX [M 1355-1361] and does not claim to test libmdbx itself.

## 3. Process-level kill harness (CI, no root)

The `xone-db` test binary runs the real `OsFile` and real MDBX in a temp dir, with a failpoint at each ID of section 1. A parent test spawns the child, the child aborts at K, the parent reopens and applies the same oracle. This models F1 only (data already in `write(2)` survives); it proves the protocol logic with real MDBX, not power loss. Also run SIGKILL at random times (section 6).

## 4. Gates by environment

| Gate | Faults | Needs | Where |
|---|---|---|---|
| G1 In-memory crash matrix (section 2) over every kill point | F2-F8 | Nothing | Every CI run. Fixed seed set; nightly with fresh seeds |
| G2 `reconcile` row tests (`crates/xone-db`) | rows D1-D14 | Nothing | Every CI run. Exists now |
| G3 Process kill with real MDBX (section 3) | F1 | Nothing | Every CI run |
| G4 Random SIGKILL while following a recorded chain | F1 | Nothing | Nightly |
| G5 ENOSPC on a small filesystem | F7 real | Loop device or a size-limited `tmpfs` mount, root | Privileged runner |
| G6 EIO and fsync error on real files | F6, F8 real | `dm-error` or `dm-flakey` (device-mapper), root | Privileged runner, nightly |
| G7 Power-loss and reordering on real MDBX and files | F2, F4, F5 real | `dm-log-writes` or a VM killed with its disk cache dropped (QEMU), root | Privileged runner, weekly. Replaces nothing in G1; checks that the model in section 2 matches a real kernel |
| G8 Same matrix on ext4 and xfs | all above | Two loop images | Privileged runner; the Gate B pass needs both |

CI-only (unprivileged) gates: G1, G2, G3, G4. Gates needing a loop device or device-mapper: G5-G8. The issue's stop condition, "full matrix passes on two filesystems", is met by G1 plus G3 plus G5-G8 on ext4 and xfs. The in-memory model is not a filesystem; its claim is limited to what ADR section 3 declares. Any disagreement between G1 and G7 means the model is wrong, and the model is fixed first.

Mount options, kernel version and filesystem type are recorded per run (`xone-rs-durability-signer-gate` operating rule 3).

## 5. Fsync-failure and fail-stop tests

1. Inject `EIO` on `sync_all` at each of K10-K13 and on `Env::commit`. Expect: process exits non-zero, one alarm line, `FSYNC_FAILED` written with the boot id, no in-process retry (assert `sync_all` was called once per file).
2. Restart in the same simulated boot: expect refusal `FsyncFailedSameBoot`.
3. Restart after a simulated reboot (page cache dropped, boot id changed): expect recovery by the decision table or a typed refusal.
4. ENOSPC on write at K06-K09 and K14: expect `Degraded(NoSpace)`, tail truncated to the committed `len`, no orphan chunk file, journal kept, reads still served, `may_sign()` false; then free space and expect import to resume and `may_sign()` true.
5. Cleanup failure (second `ENOSPC` during truncate): expect fail-stop.
6. Signer interface: with a stub signer calling `durability().may_sign()` before each signature, count signatures released while `Degraded` over every case above. Expected 0.

## 6. Random and long runs (supplement)

- 100 SIGKILLs at random times while following the tip, at least 10 in each class from `xone-rs-restart-unwind-gate` step 3 (mid-epoch, within 5 blocks of an epoch switch, during bootstrap, with a sibling head). 100 of 100 recover with no manual repair.
- Disk-full run from `xone-rs-restart-unwind-gate` step 6 on a loop device.
- 20 of 20 SIGTERM shutdowns within 5 s.

## 7. Unwind property test

Statement (proptest; fixed seed in CI, a new seed nightly; at least 10,000 cases for the gate):

> For every chain prefix of length `N` from the corpus, every `k` with `1 <= k <= K_MAX`, and every sequence of `k` further blocks that includes the forced classes below:
> let `S(n)` be the *full storage image at head `n`*: the state tables (`PlainAccountState`, `PlainStorageState`, `Bytecodes`), the state root recomputed from them, `CanonicalHeaders`, `HeaderNumbers`, `SegIdx`, every consensus table, and the committed content of the three segments up to the manifest `len`, with `Meta[gen]` and the changesets excluded.
> Then: execute blocks `N+1..N+k` (commit each), `unwind_to(N)`: `S(N)` after the unwind equals `S(N)` captured before executing, byte for byte, in every table and segment listed. Then re-execute the same `k` blocks: `S(N+k)` equals the first run's `S(N+k)`.
> The equality also holds after a restart (`reconcile` and reopen) inserted at any point: after the unwind, and between the unwind and the re-execution.

Generator and forced classes. Each generated `k`-block range must contain, over the whole run and in at least 20% of cases each:

- `0x89` special transactions (sign tx to the BlockSigners contract; checklist row B2 in `xone-rs-consensus-parity`) as the sender's first, middle and last transaction of a block, and from a sender whose nonce is touched by nothing else;
- `0x90` special transactions (the second special transaction class named in `xone-rs-restart-unwind-gate`) in the same positions;
- a reward checkpoint block, an epoch switch, and a block with a wide-balance credit (boundary matrix of issue #1 P0-1, once it exists);
- an empty block, a block whose receipts list is empty, a block that crosses a `BLOCKS_PER_FILE` chunk boundary (set `BLOCKS_PER_FILE` small in the test build so this is common), and a sibling-head reorg (unwind then a different `k`-block branch).

Checks run on both a clean unwind and on a crash at each of U1a-U5b.

Pass: 0 failures in at least 10,000 cases, plus every forced class covered at least 200 times (the harness prints the counts). A failure prints `(seed, N, k, block list)` and the first differing table and key.

Note on timing: the property test needs the executor and consensus tables. Until then Gate A proves it only on a synthetic state model with the same tables; Gate B reruns it on the real executor. Replace the synthetic model with real blocks without changing the statement.

## 8. Report format (what the gate attaches to issue #11)

Boundary list with one result row per ID in section 1 and per fault; filesystem model and mounts; kernel version; seeds and minimized traces for every failure; commit SHA; the fault-matrix table (boundary by fault to outcome) that the issue's "Images" section asks for; counts for the property test.
