# Block import: verify, execute on the parent's state, commit or store

## Rule

`Chain::insert(block, now)`:

1. **Parent.** The parent header is read by hash (canonical, side or stateless). Absent: `UnknownParent` (geth `ErrUnknownAncestor`).
2. **Header verification, before the import lock.** The `HeaderCheck` (production: `xone_consensus::verify::HeaderVerifier` with the epoch context) reads the chain through a `ChainRead` that takes the store lock per call. It runs with no import lock held. A reader never waits for the import lock, and the import lock never waits for a reader.
3. **Import lock.** One mutex serialises insert, reorg and set-head.
4. **Known.** A block with a stored execution is not executed again. Canonical at or below the head: `Known`. Otherwise it goes to fork choice (`fork-choice.md`).
5. **Body check, then execute.** `BodyVerifier::verify_body` (uncles, transaction root, withdrawals, blob gas, access list presence, size and signing-transaction caps; geth `ValidateBody`) runs before execution: a body that does not match its header is never executed. Then **execute on the block's own parent state** (`Store::state_at(parent_hash)`: canonical inside the retained window, or the side branch as overlays). Never on the head's state (xOneGo `5c7be429`, `e8c845bd`).
6. **Verify, with no branch and no switch:** `BodyVerifier::verify_receipts` (receipt count, bloom, receipt root), then gas used, access list hash, state root. A failure leaves the store untouched.
7. **Commit, side or reorg** by the fork choice. A canonical commit also persists the committed V2 block (`reorg-and-set-head.md`).
8. **Parent state not available** (pruned, or the parent is itself stateless): the block is stored without state, at most 8 children per parent, and is never adopted. A stateless block with the state root of the canonical block at its height is refused ("ghost state"). geth re-executes such a branch from the nearest ancestor whose state it can recover; this store cannot recover pruned state, and the committed-block floor refuses any such fork below the finalised point anyway.

## Source

- `core/blockchain.go:2510` `insertChain`; `:2551-2569` known blocks; `:2588-2592` `ErrPrunedAncestor` to `insertSideChain`.
- `core/blockchain.go:2928` `ProcessBlock`; `:3170-3172` write state, set head.
- `core/block_validator.go:52-190` `ValidateBody` (`:65` known, `:76`, `:79` uncle and transaction roots, `:159-181` parent availability), `:198-259` `ValidateState` (`:206-214` gas, `:217` bloom, `:227` receipt root, `:238-252` access list, `:258` state root).
- `core/blockchain.go:3202-3347` `insertSideChain`; `:3211-3231` ghost-state; `:3236-3256` the cap; `:153` `sidechainChildCap = 8` (CertiK XDC-19).
- `core/blockchain.go:2252` `writeBlockWithState` (block, receipts, state, TD in one write).

## Networks

All.

## Rust

`xone_sync::chain::{Chain, insert::{Outcome, InsertError}, side_index}`. Execution is behind `xone_sync::executor::BlockExecutor` (`parent: &dyn StateRead`, header, body; returns `Execution { diff, receipts, state_root, gas_used, access_list_hash }`). `HeaderCheck` is a trait so that the node supplies the context; a closure implements it. `Store` is the persistence and state backend (`store.rs`); `testkit::MemStore` is the in-memory implementation with the ADR-005 kill points, `testkit::VectorExecutor` replays recorded executions and also asserts the parent state each recording was made on (a block run on the wrong parent fails like a state root mismatch).

`xone-consensus` `BodyVerifier::verify` is now `verify_body` followed by `verify_receipts` (a split with no change of behaviour or error order) so that the importer can run the first half before execution.

No lock is held across a consensus read: `tests/locks.rs` runs a cold verification (reads, then waits for another insert to finish) concurrently with inserts, in V1 and V2, with a 5 s bound; the same scenario under one lock times out (control), and moving the check under the lock in `insert` fails the test (checked by hand).

`tests/no_switch.rs` is the grep test: the `BodyVerifier` call follows the execution result in a straight line, the gas, access list and root comparisons follow it, and `settle` is reached only through that path or the known-record path. No `cfg`, no `env::var`, no Cargo feature, no "skip", "unchecked" or "bypass" exists in the crate. `tests/chain.rs::every_check_runs_on_every_import` corrupts each field at the first block and in the middle of a run, in V1 and V2, and requires the typed error and an unchanged store.

## Vectors

`tests/chain.rs` (import, checks, side blocks, caps, ghost state), `tests/shapes.rs` (real receipts and bodies: a flipped byte of a stored receipt or transaction is a `Body` error; net2040 access list hash).

## Not covered

- `ProcessBlock` details of geth that belong to the executor: requests hash (XDC has none), block access list validation (`enc.Validate`, `:249`), tracing, witnesses.
- The bulk-sync tolerances of the geth fork (`checkpointSyncNoState`, `TrustedSyncCheckpoints`, the first 10,000 blocks that may lack parent state). There is no switch for them.
- Header verification context itself (epoch switch, gap snapshot, penalties). The manager only guarantees when it runs.
