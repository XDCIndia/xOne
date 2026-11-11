# Restart: resume a consistent head or refuse with a typed error

## Rule

On open, before any block is imported:

1. Read the committed generation and manifest, scan the segment tails, and let `xone_db::reconcile` decide (ADR-005 section 7). `Refuse` is returned as `ResumeError::Refused(reason)`; `Repair` is applied (truncate, quarantine when far ahead); orphan chunk files and a stale or half-written journal are removed (idempotent publication). The journal is never replayed: the block is imported again from the network.
2. `Fresh` (generation 0, no manifest, no segment file) is accepted **only if the store holds no record at all**. A datadir that holds records but no manifest is not new: refused. There is no path that returns head 0 for a datadir that holds data.
3. The head is the manifest head. It must be the canonical block at that number (`MissingCanonical { number }`, `HeadMismatch`), its header must exist (`MissingHeader`), and the canonical index of the last 256 blocks must be complete and link by parent hash (`MissingCanonical { number }`, `BrokenLink`). The head state must open (`MissingState`). Every error names the record.
4. Only then the manager starts. `Chain::open` returns the chain and `Resumed::Head { number, hash, repaired }`.

The committed block is read from the same transaction as the head, so it is never behind the head (`reorg-and-set-head.md`).

The xOneGo record: `headBlock=0` on a non-empty datadir (2026-11-05), then `resume: no canonical hash for block #618613` (2026-11-09); recovery was a datadir wipe. Here the second case is the typed refusal `MissingCanonical { number: 618613 }`, and the first cannot occur.

## Source

- `docs/adr/ADR-005-storage-and-publication.md` sections 5, 7 (decision table D1-D14) and 8; `crates/xone-db/src/recover.rs` `reconcile`.
- geth `core/blockchain.go:912-926` `loadLastState`: a missing head hash ("Empty database, resetting chain") or a missing head header ("Head header missing, resetting chain") calls `bc.Reset()`, which restarts from genesis. This is the head-0 start of xOneGo #720, and xOne does the opposite: it refuses. geth then rewinds to a block with state (`:927-`); xOne repairs only by `reconcile` and never rewinds on its own.
- xOneGo lessons: `f10a110d`, `6ef0f16b`, `578f2765`, `521c4b41`, `36aab126`, `bcacced8` (restart fixes on master), `c0f707d5` (branch only), #720 (open).

## Networks

All.

## Rust

`xone_sync::chain::resume::{resume, Resumed, ResumeError, RESUME_WINDOW}`, `Chain::open`, `Store::{scan, repair}`.

## Vectors

- `tests/chain.rs::a_corrupted_datadir_is_refused_naming_the_missing_record`: corrupted index at the head and below it, missing record, missing manifest, flipped manifest byte, missing segment file: each a typed error that names the missing record.
- `tests/kill.rs`: a store that crashes or fails at each of the 27 kill points (K01-K19, U1a-U5b), under `kill -9`, power loss and a torn tail, during insert (V2, V1, chunk boundary), reorg, sibling adoption and set-head: 853 crash runs. After each, `Chain::open` with no other help; the head equals the head the dead store had published; the state is that head's state; the canonical index links to the anchor; finishing the work by importing again reaches the same final head and flat state root. Every kill point is reached by at least one run.
- `kill.rs`: 100 randomized restart sequences (random mode, operations, kill points and faults; 256 crash-restarts), and a 48-block sync-like import burst killed with `kill -9` at ten points (journal, execute, segments, chunk creation, sync, transaction, commit, publish), V1 and V2. Each restart rolls back to the last published head; the canonical index equals what the dead process had published; nothing is ever wiped.

## Not covered

- The real MDBX store and the real segment writer on disk; the kill harness with a real process and `SIGKILL`, and the loop-device gates of `tests/fault/PLAN.md`. The in-memory store follows the protocol step by step and models the journal, the segment files (synced length, directory entry), the atomic commit and the cuts F1-F3; CRC, MDBX internals, F6-F8 retry behaviour and the start-up repair kill points RP1-RP3 are not modelled. Segment payloads are the header RLP and stand-ins for body and receipts, so the tail digests and lengths are real but the frames are not readable data.
