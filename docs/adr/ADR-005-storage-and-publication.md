# ADR-005: Storage and cross-store publication

Status: proposed (Gate A design review). Gate B must prove it. Issue: XDCIndia/xOne#11 (XA-06). Plan: issue #1 ADR-005 line and Revision 1 section 4.
Code: `crates/xone-db/` (traits, manifest bytes, pure `reconcile`, `mdbx`, `segments`, `fault`), `crates/xone-sync/src/mdbx/` (`MdbxStore`). Fault plan: `tests/fault/PLAN.md`. Gate B implementation notes: section 15.

## 0. How to read the sources

Every claim about MDBX, the filesystem, or reth carries a tag.

- **[M n]** libmdbx `mdbx.h` line n. File: `crates/storage/libmdbx-rs/mdbx-sys/libmdbx/mdbx.h` in the reth checkout `/mnt/data/tmp/reth-apothem-spec` (libmdbx `VERSION.json`: v0.13.12). The vendored crate is not in `~/.cargo/registry`; this checkout is the only local copy.
- **[R path:n]** reth source, same checkout, path under `crates/`. Commit `0995c2519745622cb76b78217118edde2d78545a` (`v2.9.0-xdc.1-28-g0995c25`, workspace version 2.9.0). This is not the `v2.10.1` Storage V2 pin named in Revision 1 section 8. No claim here describes Storage V2.
- **[F-man]** `fsync(2)`, https://man7.org/linux/man-pages/man2/fsync.2.html (read 2026-11-11).
- **[F-pg]** PostgreSQL wiki "Fsync Errors" (fsyncgate 2018), https://wiki.postgresql.org/wiki/Fsync_Errors (read 2026-11-11).
- **[F-atc]** Rebello et al., "Can Applications Recover from fsync Failures?", USENIX ATC 2020, https://www.usenix.org/conference/atc20/presentation/rebello. Cited by [F-pg]. I did not read the paper body in this session; it is cited only for the topic.
- **[D]** a design decision of this ADR. It is a proposal. No test has run.
- **[U]** unverified. Stated as an assumption, to be tested in Gate B.

No primary source for ext4 or xfs ordering guarantees was gathered. Section 3 therefore declares a model and says the gate tests it, instead of claiming what ext4 or xfs do.

## 1. Stores

| Store | Holds | Authority |
|---|---|---|
| MDBX env `datadir/chain.mdbx` | state, changesets, canonical index, segment index, consensus tables, `Meta` | The head and the generation |
| Static files `datadir/static/{headers,bodies,receipts}/` | one frame per block per segment | None. Valid only up to the committed manifest |
| Journal `datadir/journal/intent` | the block being imported, or the unwind target | None. An input log, never a state log |
| Safety record (XA-07) | highest voted rounds | Out of scope. Separate store. Interface in section 11 |

MDBX tables (names are the contract; consensus tables are owned by the consensus items and named provisionally):

- `Meta`: bytes to bytes. Keys `gen` (u64 LE) and `manifest` (257 bytes, section 6.3). Both are written in every commit transaction.
- `CanonicalHeaders` (u64 BE to B256), `HeaderNumbers` (B256 to u64 BE).
- `SegIdx`: key `segment u8 || block u64 BE`, value frame start offset u64 LE. It serves reads by number and gives the cut offset for unwind.
- State: `PlainAccountState` (address to account RLP, wide balance included), `PlainStorageState`, `Bytecodes`.
- Changesets: `AccountChangeSets` (key `block u64 BE || address`, value the full prior account record), `StorageChangeSets` (key `block || address || slot`, value prior value).
- Consensus: `ConsensusEpoch`, `ConsensusSnapshot`, and any table the consensus items add.

**Unwind rule (invariant for every table):** a table is either keyed with the block number first, so unwind deletes keys above `n`, or it has a changeset that stores the whole prior record. Changesets store whole records, never deltas, so a nonce and a balance cannot be reverted separately. This is the lesson of the reth-xdc sign-tx nonce report in issue #11 (reported, not found in source, unverified).

Segment files: `static/<segment>/<first_block as 10 digits>.seg`, `BLOCKS_PER_FILE = 100_000` [D] (reth default 500_000: `static-file/types/src/lib.rs:35`). The active file of a segment is the one whose range holds `manifest.entry.last_block`.

## 2. Terms

- **Generation `g`**: u64, starts at 1 at genesis commit, `+1` per MDBX commit that moves the head (commit or unwind). One block normally means one generation. A batch commit (open question 6) means one generation for `k` blocks.
- **Frame**: one block's record in one segment (section 6.1).
- **Committed length `len`**: the end offset of the last committed frame in a segment file, taken from the manifest.
- **Published**: a generation is published when its MDBX commit has returned success. From then, and not before, readers, RPC, peers and the signer interface may see that head.
- **Staged**: bytes in a segment file beyond `len`. Readers never read beyond `len`. Staged bytes are invisible by construction, not by file name.

## 3. Declared filesystem and device model

The protocol is correct under this model. A fault outside it is not claimed to be handled.

Modelled (each has a test in `tests/fault/PLAN.md`):

| # | Fault | Meaning |
|---|---|---|
| F1 | Process crash | `kill -9`, abort, OOM kill. Data already passed to `write(2)` survives |
| F2 | Power loss or kernel panic, fsync honoured | Data covered by a returned `fsync` survives. Data not covered by one may be absent in whole or in part |
| F3 | Torn or short write | A crash inside a write leaves any prefix, cut at 512-byte granularity or finer; `write(2)` may return a short count |
| F4 | Reordered writes | Between two successful fsync barriers, unsynced writes of one or several files reach the device in any order and any subset |
| F5 | Delayed flush | Unsynced data reaches the device arbitrarily late, or never before a crash |
| F6 | fsync error (EIO, ENOSPC) | The call fails once. The state of every page of that file is unknown afterwards ([F-man] ERRORS, [F-pg] "retrying fsync() is not OK") |
| F7 | ENOSPC on write, extend, truncate or create | No data is lost that was reported durable |
| F8 | EIO on read or write | As F6 for writes; a read error is a typed refusal |

Not modelled (a release note must say so):

| # | Not modelled | Reason |
|---|---|---|
| N1 | fsync returns success but data is not durable (lying cache, firmware bug) | No software protocol can detect all of it. The tail digest (section 7) catches one case at start-up |
| N2 | Silent corruption of data synced earlier | Frame CRC catches it on read; it is not repaired |
| N3 | Network filesystems, RAID write holes, multi-device volumes | Not a supported datadir |
| N4 | Directory entry lost after a successful directory fsync | Assumed not to happen |
| N5 | A second process writing the datadir | Prevented by an exclusive `flock` on `datadir/LOCK` [D] |
| N6 | An attacker with write access to the datadir | Not a storage fault |
| N7 | Atomic `rename` after crash (tmp file fsynced first) | **[U]** assumed for ext4 and xfs; Gate B tests it on both |

Facts used from the sources:

- A returned `fsync` flushes file data and metadata to the device, "including a disk cache if present" [F-man DESCRIPTION].
- `fsync` on a file does not necessarily persist its directory entry; the directory needs its own `fsync` [F-man DESCRIPTION]. So: create or rename, then fsync the directory.
- A size change by `ftruncate` needs a metadata flush [F-man, fdatasync paragraph]. All syncs here are `fsync` (`File::sync_all`), not `fdatasync`.
- After a write-back error the kernel may report it only once; since Linux 4.13 the error is reported to all descriptors open at the time, but a retry of `fsync` must not be trusted [F-man EIO; F-pg "Linux 4.14 and >= 4.16"].

## 4. MDBX sync mode: `SyncMode::Durable` (`MDBX_SYNC_DURABLE`)

Decision [D]: open the environment with `Durable`. WRITEMAP stays off in v1 (open question 3).

Why:

1. `Durable` writes the meta page after the data pages are flushed, which "guarantees the integrity of the database in the event of a crash at any time" [M 1355-1361; same text in `libmdbx-rs/src/flags.rs:9-12`]. A returned commit is durable. The protocol (section 5) needs exactly that: the MDBX commit is the publication point, and a head that has been served must not go backwards after power loss.
2. `NoMetaSync` keeps atomicity, consistency and isolation but "a system crash may undo the last committed transaction" [M 1365-1379; `flags.rs:15-27`]. A commit that returned would not be durable. Rejected.
3. `SafeNoSync` also rolls back to the last steady commit after a crash [M 1395-1430]. `UtterlyNoSync` can corrupt the database if disk write order is not preserved, and the header says "please do not use other modes until you have studied all the details" [M 1359-1361, 1450-1480; `flags.rs:76-103`]. Rejected.
4. The reth checkout shows what a weak mode does to a cross-store design. Its comment says that after power loss MDBX "discards all weak commits and rolls back to the last steady one, while static files ... sync on every commit and stay ahead", and the rollback can land below data that pruning already deleted, "which startup healing cannot repair" [R storage/db/src/implementation/mdbx/mod.rs:57-65]. reth bounds this with a 60-second sync period (`NON_DURABLE_SYNC_PERIOD`) for the weak modes (mod.rs:463-465); its default is `Durable` (mod.rs:158). xOne avoids the class by using `Durable`.
5. Cost: about 8 fsync calls per block (journal 2, segments 3, directory 1, MDBX 2). At a 2 s block time this is acceptable on a follower or producer [D, not measured]. Bulk replay needs batching (open question 6).

Consequence: "MDBX ahead of segments" cannot happen with honest fsync (section 7). Under F2 only "segments ahead of MDBX" occurs.

## 5. Commit order per block

Order: journal, execute, stage segments, fsync segments, fsync directory, MDBX transaction with generation and manifest, commit, publish. The kill point IDs are used by `tests/fault/PLAN.md`.

| Step | Action | Kill points (after the action unless stated) |
|---|---|---|
| S1 journal | Write `journal/intent.tmp` (6.4), fsync it, rename to `intent`, fsync `journal/` | K01 before write; K02 written unsynced; K03 fsynced, not renamed; K04 renamed, dir not synced |
| S2 execute | Execute the block into an open MDBX write transaction. No durable effect | K05 mid-execution |
| S3 stage | For each segment in order headers, bodies, receipts: verify `file_len == len` of the committed manifest (else fail-stop, section 9), append the frame. If the frame crosses `BLOCKS_PER_FILE`, create the next chunk file with its header | K06 headers frame written; K07 bodies; K08 receipts; K09 new chunk file created, dir not synced |
| S4 sync | `fsync` each segment file. Then `fsync` each segment directory | K10 headers fsynced; K11 bodies; K12 receipts; K13 directories fsynced |
| S5 txn | In the MDBX write transaction also write `SegIdx`, canonical index, `Meta[gen] = g+1`, `Meta[manifest]` with entries `len` = new end offsets and `tail_digest` = keccak256 of the new frames | K14 transaction complete, before commit |
| S6 commit | MDBX commit under `Durable` | K15 error injected inside commit (loop-device gate only); K16 commit returned |
| S7 publish | Swap the in-memory visible lengths and head; delete `journal/intent`; fsync `journal/` | K17 after swap; K18 journal unlinked, dir not synced; K19 dir synced |

Rules:

- No frame is read by any reader beyond the manifest `len`. Staging needs no rename because visibility is the manifest, which is inside the MDBX commit [D]. This replaces the "rename or visible-length update" alternative of Revision 1 section 4 item 3 with the visible-length variant.
- **Durable publication marker** [D]: it is the `Meta[manifest]` record committed at S6, magic `XMF1`, whose `generation` equals `Meta[gen]`. There is no second marker. A second marker would need its own cross-store ordering. The head is served only after S6 returns.
- **Idempotent publication**: S7 reads nothing new. After a restart, section 7 rebuilds the visible lengths and head from `Meta[manifest]`, then deletes any `journal/intent` whose generation is `<=` the committed one. Running S7 twice, or after a crash at K17-K19, gives the same state.
- Journal replay: if `journal/intent` has generation `g+1` where `g` is the committed generation, the importer may re-execute the stored block through the normal import path. The journal's effects are never applied directly.
- The journal is deleted last so that a crash after S6 and before S7 leaves a journal with generation `<= g`, which is deleted without replay.

## 6. Formats

### 6.1 Segment file

Header, 20 bytes: `"XSEG"` (4) `| version u16 LE | segment u8 | pad u8 (0) | first_block u64 LE | crc32c u32 LE` over the preceding 16 bytes. Then frames, back to back.

Frame: `payload_len u32 LE | block u64 LE | payload | crc32c u32 LE` over `payload_len || block || payload`. Overhead 16 bytes. Payload: header RLP for `headers`, body RLP for `bodies`, receipts RLP list for `receipts`. The frame start offset of every block is in `SegIdx`.

### 6.2 Checksums [D]

- Frame and file header: CRC-32C (Castagnoli). It detects torn and short writes and random bit damage on every read. It is not collision resistant and not meant to be.
- Manifest checksum and `tail_digest`: keccak256 (32 bytes). It binds the manifest to exact bytes, and it is already in the dependency tree (`xone-primitives` uses `alloy_primitives::keccak256`).
- MDBX itself is not relied on for payload integrity beyond its own commit protocol [M 1355-1361].

### 6.3 Manifest (in `Meta[manifest]`, implemented in `crates/xone-db/src/manifest.rs`)

All integers little endian. 257 bytes.

```
off  len  field
0    4    magic "XMF1"
4    2    format version (1)
6    8    generation
14   8    head_number
22   32   head_hash
54   57   entry headers   : segment u8 (=1) | first_block u64 | last_block u64 | len u64 | tail_digest 32
111  57   entry bodies    : segment u8 (=2) | ...
168  57   entry receipts  : segment u8 (=3) | ...
225  32   keccak256(bytes 0..225)
```

`len` is the end offset of the last committed frame. `tail_digest` is keccak256 of that whole frame. `first_block` is the first retained block of the segment (pruning, section 10). Every entry has `last_block == head_number`.

`Meta[gen]` is the generation as u64 LE (8 bytes). It repeats the manifest's generation on purpose: a difference means the MDBX file itself is damaged (decision D6).

### 6.4 Journal `journal/intent`

`"XJN1" | kind u8 (1 commit, 2 unwind) | generation u64 | block_number u64 | block_hash 32 | payload_len u32 | payload | crc32c u32`. For `commit` the payload is the block RLP. For `unwind` the payload is empty and `block_number` is the target.

## 7. Restart reconciliation

Start-up preconditions, outside the pure function (all fail with a typed error before reconcile):

- P1. Take the `flock` on `datadir/LOCK`.
- P2. If `datadir/FSYNC_FAILED` exists and its boot id equals `/proc/sys/kernel/random/boot_id`: refuse (`FsyncFailedSameBoot`). Reason in section 9.
- P3. Open MDBX read-write, read `Meta[gen]` and `Meta[manifest]`.
- P4. Scan each segment: file present, length, keccak256 of the frame ending at manifest `len`, and the count of complete CRC-valid frames beyond `len`.

Orphan chunk files (a crash at K09 to K14 can leave a new `.seg` whose range starts after the active chunk) carry no committed data. The scanner lists them; start-up unlinks them and fsyncs the directory, outside the table.

Then `xone_db::reconcile(mdbx_generation, manifest_state, tails)` decides. It is a pure function. First matching row wins. A refusal for any segment overrides repairs for the others.

### Decision table

"MDBX ahead" means MDBX committed data the segments do not hold. "Segments ahead" means segments hold bytes MDBX did not commit.

| Row | Condition | Case | Outcome |
|---|---|---|---|
| D1 | generation 0, manifest missing, no segment file | new datadir | `Fresh` |
| D2 | generation 0, manifest missing, some segment file present | segments with no authority | Refuse `OrphanSegments` (never cut data without an MDBX authority) |
| D3 | generation > 0, manifest missing | missing manifest | Refuse `ManifestMissing` |
| D4 | manifest bad magic, length, checksum or layout | corrupt manifest | Refuse `ManifestCorrupt` |
| D5 | manifest version differs from the binary's | format version | Refuse `FormatVersion(found)` (section 12) |
| D6 | `manifest.generation != Meta[gen]` | damaged MDBX | Refuse `GenerationMismatch` |
| D7 | an entry's `last_block != head_number` | inconsistent manifest | Refuse `ManifestInconsistent` |
| D8 | segment file missing | MDBX ahead | Refuse `SegmentMissing(seg)` |
| D9 | `file_len < len` | MDBX ahead (lost synced data, N1) | Refuse `SegmentShort(seg)` |
| D10 | `file_len >= len` but tail digest does not match | torn or reordered write inside committed data | Refuse `SegmentCorrupt(seg)` |
| D11 | `file_len == len` for all segments | consistent | `Ready` (idempotent publication only) |
| D12 | `file_len > len`, no complete extra frame | torn segment tail | Repair: truncate to `len` |
| D13 | `file_len > len`, 1 to 128 complete extra frames | segments ahead: staged block not committed (kill in S3-S5), or unwind remnant | Repair: truncate to `len` |
| D14 | more than 128 complete extra frames | segments far ahead: stale MDBX restore or rollback | Repair: copy tail to `quarantine/`, fsync, then truncate |

Combinations the issue lists:

| Combination | Rows | Result |
|---|---|---|
| MDBX ahead of segments, any form | D8, D9, D10 | Refuse. With honest fsync this needs N1 or N2. Recovery is an operator action: restore the datadir or resync. The node does not lower MDBX to the segment head, because that would need headers and bodies that are gone [D, open question 4] |
| Segments ahead of MDBX | D12, D13, D14 | Truncate (quarantine when far ahead). The block is re-imported from the journal or the network |
| Torn segment tail | D12 (or D13 with a torn part after whole frames) | Truncate |
| Missing manifest | D2, D3 | Refuse |
| Crash after commit before publication (K16-K19) | D11 | `Ready`; publication is redone |
| Crash inside unwind after commit, before segment truncation (U3-U4) | D13 | Truncate to the new `len` |

Truncate applies `ftruncate`, then `fsync` of the file, then `fsync` of its directory. The repair is idempotent: a crash during repair repeats the same rows.

Limits [D]: start-up verifies the committed tail frame, not the whole file. Every frame CRC is checked on read. A full verify command (`xone db verify`) walks all frames and is a Gate B tool.

Reth comparison (why the table looks like this):

- reth writes static files first and the MDBX transaction second. `DatabaseProvider::commit` calls `static_file_provider.finalize()` then pending RocksDB batches then `tx.commit()` [R storage/provider/src/providers/database/provider.rs:4034-4045]. Its writer syncs data and offsets (`nippy-jar/src/writer.rs:371-376`), then writes the config last with a temp file, `fsync`, `rename`, and a parent directory `fsync` [R nippy-jar/src/lib.rs:280; fs-util/src/lib.rs:328-368].
- Its consistency checker assumes "the configuration is always the last to be updated" and heals an offsets or data file that is longer or shorter than the config [R nippy-jar/src/consistency.rs:7-10, 41-120].
- The disagreement window is between `finalize()` and `tx.commit()`. A crash there leaves the segment ahead of the stage checkpoint. reth's `ensure_invariants` then compares the static file's highest block with the stage checkpoint: checkpoint ahead gives an unwind target; checkpoint behind prunes the extra static rows [R storage/provider/src/providers/static_file/manager.rs:1735-1860, comments at 1722-1731]. xOne's D12-D14 are the same repair, but the authority is the committed manifest and not a checkpoint comparison.
- Unwind in reth commits MDBX first, then the static files: `commit_unwind` is `tx.commit()` then `static_file_provider.commit()` [R database/provider.rs:285-303]. xOne keeps this order for unwind (section 8) and makes the leftover a normal D13 case.
- I found no directory `fsync` after creation of a new data file in `nippy-jar/src/writer.rs` (the data and offsets files are opened with `OpenOptions` at lines 137 and 145). **[U]** I did not read all callers. xOne's S4 does it explicitly.

## 8. Atomic unwind

`unwind_to(n)` where `retained_first <= n < head`. One MDBX transaction covers state, changesets, canonical index, `SegIdx`, receipts bookkeeping and every consensus table, so there is no partial unwind [D].

| Step | Action | Kill points |
|---|---|---|
| U1 | Journal `unwind` with target `n` (as S1) | U1a written; U1b renamed |
| U2 | Read-only: read `SegIdx[seg, n+1].start` for each segment (the new `len`), read frame `n`, verify its CRC, compute `tail_digest` | none (no writes) |
| U3 | One MDBX write transaction: for blocks `head` down to `n+1`, restore prior records from `AccountChangeSets` and `StorageChangeSets`, delete those changeset keys, delete keys above `n` in `CanonicalHeaders`, `HeaderNumbers`, `SegIdx` and every block-keyed consensus table, write `Meta[gen] = g+1` and the manifest with the new `len`, `tail_digest`, `head_number`, `head_hash`. Commit | U3a before commit; U3b commit returned |
| U4 | `ftruncate` each segment to the new `len`, `fsync` file, `fsync` directory | U4a after first truncate; U4b after all |
| U5 | Delete journal, fsync `journal/` | U5a; U5b |

Receipts are in a segment, so their unwind is the manifest `len`; readers stop seeing them at U3b. A crash at U3b..U4b leaves segments longer than the manifest: row D13, repaired by truncation. The next `commit_block` after an unwind starts from a truncated file (S3 checks `file_len == len`).

The safety record is never changed by unwind (section 11).

Bound [D, open question 5]: the single transaction holds the changesets of all `k` blocks. A cap `MAX_ATOMIC_UNWIND` is needed for operator `SetHead` of very deep targets. Reorgs on XDPoS V2 are shallow.

## 9. Failure policy: ENOSPC, EIO, failed fsync

State is `Durability` (`xone-db`): `Durable` or `Degraded(Reconciling | NoSpace | Io | FsyncFailed)`. `may_sign()` is true only for `Durable`.

| Event | Action |
|---|---|
| ENOSPC from `write`, `ftruncate`, create or MDBX write before any sync of this attempt | Abort the MDBX transaction, truncate segments to the committed `len`, remove an orphan chunk file, keep the journal, set `Degraded(NoSpace)`, log one alarm line `xone-db: degraded cause=no_space`. Reads continue. Retry when free space is above a reserve [D, value open]. If cleanup itself fails: fail-stop |
| `fsync`, `fdatasync` or MDBX commit returns any error (including ENOSPC and EIO) | Fail-stop: set `Degraded(FsyncFailed or Io)`, write `datadir/FSYNC_FAILED` containing the boot id (best effort), log one alarm line, exit non-zero. No retry in process |
| EIO on write | Same as the line above |
| EIO on read | Typed error to the caller; the block is not served. A start-up read error is a refusal |
| MDBX commit outcome unknown (error returned) | Exit. The restart reads `Meta[manifest]`; the manifest says whether the generation committed |

Why a failed sync is fatal: after a failed write-back the kernel may mark pages clean, so a retry can report success while data is lost, and older kernels could drop the error entirely [F-pg "Linux < 4.13", "Linux 4.14 and >= 4.16": "you only get the error once (so retrying fsync() is not OK)"; F-man EIO]. PostgreSQL moved to PANIC on fsync failure for the same reason [F-pg "Current status"]. The question of what a restart in the same boot reads is a page-cache question: after a failed fsync the cache may hold bytes the device never got, so a CRC check that reads through the cache can pass. P2 refuses until the next boot, when the cache is empty and P4 reads the device **[U: the same-boot cache behaviour is reasoned from [F-pg], not measured]**. If `FSYNC_FAILED` could not be written, this guard is absent (open question 7).

"Resume after space is freed": NoSpace resumes in process. Fail-stop paths resume by restart, which runs section 7.

## 10. Pruning

Pruning removes whole chunk files only, below a chunk-aligned floor. It is interruptible and resumable.

| Step | Action | Kill points |
|---|---|---|
| P-1 | MDBX transaction: delete `SegIdx` and block-keyed rows below the floor, set `first_block` in the manifest, `gen+1`. Commit | PR1 before commit; PR2 after commit |
| P-2 | Unlink each chunk file wholly below `first_block`, in ascending order | PR3 after first unlink; PR4 after last unlink |
| P-3 | `fsync` the segment directory | PR5 |

Readers stop seeing pruned blocks at PR2 (visibility is the manifest). A crash at PR2..PR5 leaves files below `first_block`; start-up deletes any chunk file whose range lies wholly below `first_block` (idempotent, and the same visibility rule). A file that straddles the floor is kept. Unwind below `first_block` is refused (`BadUnwindTarget`).

The reth hazard (MDBX rolling back below pruned data) cannot occur: the commit of P-1 is durable before any unlink, by `Durable` (section 4).

## 11. Interface to XA-07 (signer safety)

- `BlockWriter::durability() -> Durability`. The signer checks `may_sign()` before every signature. Any `Degraded` state means no signing. Reconciliation at start is `Degraded(Reconciling)`.
- A signature may refer only to a head of a published generation (section 2).
- The safety record is a separate store with its own sync discipline. `unwind_to`, `SetHead`, restore and snapshot import never lower it (Revision 1 section 5). `xone-db` has no API that touches it.

## 12. Format version, migration, downgrade

- Three version fields: manifest (u16 at offset 4), segment header (u16 at offset 4), journal magic (`XJN1`). The manifest version is the datadir version; segment headers must match it.
- Version 1 is the only version. The binary refuses any other with `FormatVersion(found)` (D5).
- **Older datadir (future)**: refuse with a migration-required error; `xone db migrate` runs offline as expand-and-contract: write new structures next to old, verify, switch by one MDBX commit that writes the new manifest, then remove old structures. It is resumable because the manifest is written last.
- **Newer datadir (downgrade)**: always refuse. No in-place downgrade exists. The way back is a backup taken before migration (both stores copied together while the node is stopped).
- Test: a manifest with version 0 and 2 and a segment header with a wrong version produce a refusal and no file change (Gate B).
- **Retention windows (XA-09, issue #14).** The first retained block of each segment is `first_block` in the manifest (section 6.3), so the manifest already records how much history a datadir holds. How much a rule needs is derived from the chain spec, not fixed: `Retention::derive` in `xone-sync` and `spec/retention.md` (Apothem: 4,500 headers, 1,799 bodies, 3,149 blocks of gap snapshots; mainnet today 1,800, 1,799, 3,149). An importer compares `first_block` with the floors it derives for the anchor and refuses a datadir that holds less (`Missing::UnsupportedHistory`); one that holds more is accepted. Rule: **retention can only grow without migration**. A new fork row or a longer `limitPenaltyEpoch` makes the derived window larger, older datadirs are refused where they fall short, and no format change is needed. **Shrinking needs a version bump**: a binary that keeps less than the manifest's meaning in an older version would make an older binary fail at its first reward block, not at open, so the manifest version increases and the older binary refuses with `FormatVersion` (D5). The anchor that binds a snapshot to its chain, checkpoint and validators is `spec/bootstrap-anchor.md`; it has its own `version` field with the same refusal rule.

## 13. Open questions for the owner

1. **Filesystem and device model.** The issue lists this as an owner question. Is section 3 accepted? Which two filesystems are the gate (proposal: ext4 and xfs, default mount options, recorded per run)? Is N7 (atomic rename) acceptable as tested, not sourced?
2. **Sync mode.** Accept `Durable` with about 8 fsyncs per block (section 4)? The alternative is `SafeNoSync` with a sync period and a design in which the head is not served until a steady commit; that is a different protocol.
3. **WRITEMAP.** Left off in v1. reth turns it on [R mdbx/mod.rs:463]; the header warns of silent corruption by stray writes [M 1139-1143]. Measure before enabling.
4. **MDBX ahead of segments.** Proposal: refuse and require restore or resync. The alternative (unwind MDBX to the last segment block) cannot rebuild lost headers and bodies. Confirm refusal.
5. **Unwind bound.** Value of `MAX_ATOMIC_UNWIND` and what an operator `SetHead` beyond it does.
6. **Bulk replay.** One generation per block costs about 8 fsyncs. Is a batched generation (k blocks, one manifest) allowed during replay? Reconcile is unchanged; the unwind granularity and kill-point set grow.
7. **Same-boot fsync-failure guard.** Accept the `FSYNC_FAILED` marker with boot id (best effort)? Should the start-up scan after a failed sync read with `O_DIRECT` or `posix_fadvise(DONTNEED)`? Neither is sourced.
8. **Constants.** `BLOCKS_PER_FILE = 100_000`, quarantine threshold 128, free-space reserve, CRC-32C and keccak256 choice (section 6.2).
9. **Path.** Issue #11 lists `docs/adr/ADR-005.md` as the allowed path; this file is `docs/adr/ADR-005-storage-and-publication.md` as instructed. Rename if the ADR index wants the short name.
10. **Reth pin.** The local checkout is reth 2.9.0 (XDC fork). Revision 1 asks for the `v2.10.1` Storage V2 text before any claim about reth's current storage. Section 7's reth lines describe this checkout only.
11. **State changeset in `commit_block`.** The trait has no state-diff parameter yet; it arrives with the executor crate.

## 14. What Gate B must prove

1. A kill test at every ID in sections 5, 8 and 10 (K01-K19, U1a-U5b, PR1-PR5): restart reaches one consistent head or a typed refusal. No visible partial file.
2. The fault matrix of `tests/fault/PLAN.md` (F1-F8 at each boundary) on two filesystems.
3. `reconcile` agrees with the real scanner on every row D1-D14, using the same fixtures.
4. The unwind property with `0x89` and `0x90` blocks forced, across state, receipts and consensus tables.
5. A sync-failure test: after injected `fsync` failure the process exits and the next start in the same boot refuses; after a simulated reboot it recovers or refuses with a typed error.
6. The release-tree check shows no fault-injection code in shipped artifacts.
7. Open questions 1 and 2 answered by the owner.

## 15. Gate B implementation notes (XA-06)

What the code does where this ADR left a choice. None changes the commit order, the manifest or the decision table.

- **Crate**: `libmdbx` 0.6.6 with `mdbx-sys` 13.11.0 (libmdbx C 0.13.11), pinned `=0.6.6`. `reth-libmdbx` is not on crates.io. `SyncMode::Durable`, WRITEMAP off (`Database<NoWriteMap>`). The crate and its build dependency `bindgen` (needs libclang) are the only `unsafe`/FFI additions; `docs/assurance/unsafe-ffi-audit-2026-11-11.md` lists them. Our crates keep `forbid(unsafe_code)`.
- **Where the code lives**: the `Store` trait is in `xone-sync` (rank 6), above `xone-db` (rank 5), so `MdbxStore` is `xone_sync::mdbx`; the environment, the tables, the segment writer and scanner and the fault disk are `xone_db::{mdbx, segments, fault}`.
- **Tables** (`xone_db::mdbx::T`): `Meta`, `CanonicalHeaders`, `HeaderNumbers`, `SegIdx`, `PlainAccountState`, `PlainStorageState`, `Bytecodes`, `HashedAccounts`, `HashedStorages`, `Preimages`, `AccountChangeSets`, `StorageChangeSets`, `BlockDiffs`, `Aux`, `SideBlocks`, `Stateless`, `ConsensusEpoch`, `ConsensusSnapshot`, `Stages`, `StagedHeaders`, `StagedBodies`, `TxLookup`. Added to section 1: `TxLookup` (transaction hash to `block u64 BE || index u32 BE`, written in each block's commit transaction, deleted with the block by an unwind; `Meta[history]` marks a datadir whose every block wrote it), the node's rows in `ConsensusEpoch` (a V2 epoch switch: hash, round, epoch) and `ConsensusSnapshot` (a V2 gap pool: the candidates), both block-keyed, `BlockDiffs` (the new values of each block inside the retained window, kept so that an unwind with `Unwound::Keep` and a reorg can re-adopt a block), `Aux` (`Record::aux`, kept for every canonical block: executors restore their context from it), the hashed tables and `Preimages` (snap serves and heals by hashed key). The committed V2 block is `Meta[committed]`. The safety record is `MdbxSafety` in its own environment (`safety.mdbx`, ADR-010).
- **Chunk files** are named by the chunk-aligned first block (`block / BLOCKS_PER_FILE * BLOCKS_PER_FILE`); the segment header repeats it. The first file of a datadir that starts at an anchor holds its first frame at offset 20. Unwind computes the new `len` as `SegIdx[n].start + frame length` (equal to `SegIdx[n+1].start` inside a chunk, and correct across a chunk boundary).
- **State reads** come from an in-memory `StateProvider` rebuilt from the tables at start-up (after `repair`) and checked against the head's state root. The tables are written in the commit transaction from the diff, not from the model. ponytail: the flat state and the trie are memory-resident until the trie has tables.
- **Journal**: written and deleted as in sections 5 and 8; start-up deletes it whatever its generation and never replays it (the block is re-imported from the network or the caller).
- **First commit of a datadir** (`init_anchor`) goes through the same steps. A kill before it returns that left segment files is row D2 (refuse, `OrphanSegments`): the datadir holds nothing and an operator removes it.
- **Failure policy**: ENOSPC on a write or truncate with a clean cleanup is `Degraded(NoSpace)` and the next write retries; every failed sync or MDBX commit, EIO, a failed cleanup and any failure after the commit returned is fail-stop with `FSYNC_FAILED` holding the boot id. `Reconciling` holds from open until `repair` (or the anchor commit) finishes.
- **Format version**: a segment header of another version is reported to `reconcile` as `ManifestState::Invalid(Version(v))`, so it refuses as `FormatVersion(v)` with no file change.
- **Pruning** (`prune_to`) requires the floor to be chunk aligned and not above the oldest openable block, so a pruned block never has a state or a changeset that is still served.
- **Measured** (Apothem blocks 0 to 1,800 through the node on this host): see the report of the XA-06 Gate B run attached to issue #11; fsync calls per block are counted with `strace -f -c`.
