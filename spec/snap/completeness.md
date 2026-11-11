# snap completeness: the state is not the whole pivot

## Rule

A snap-synced node is complete only when it holds everything that the first block after the pivot needs, not when the pivot state is downloaded. The first block above the pivot can be an epoch switch (a round can skip to the next epoch at any block), and the reward and penalty hooks of that block read blocks, receipts and gap snapshots below the pivot. Without them the node computes totalSigners = 0, pays nothing, and its state root diverges at the first reward. So the driver refuses to call the sync complete until every range of the pivot plan is held (`client::finish`, `SyncError::Incomplete`).

### What is needed: `bootstrap::pivot_plan`

`xone_sync::bootstrap::pivot_plan(spec, anchor)` returns, for the pivot `P`, one `Need` per class: a range of headers, of bodies and of receipts (each from the retention floor to `P`), one `GapSnapshot` per gap block in the window, and the `State` of `P`. The arithmetic and the source line of every rule are in `spec/retention.md`; it is a function of the chain spec and `P` only. On Apothem at `P = 83,600,000` (the first tiered-reward block): headers `P+1-4500 ..= P`, bodies `P+1-1799 ..= P`, no receipts, gap snapshots at 83,596,950, 83,597,850, 83,598,750 and 83,599,650, and the state of `P` (`crates/xone-sync/tests/bootstrap.rs`, `plan_at_the_upgrade_lists_the_first_post_pivot_reward_inputs`).

### What geth-XDC does

- `PrimeAncestorHeaders` writes a contiguous, ascending, parent-linked window of headers below the anchor window, so that the V2 engine's walk back to the last epoch switch (the header that carries the active masternodes) finds them; without them it fails `verifyHeader` with an empty masternode list (`eth/downloader/downloader.go:636-712`, function at `:658`). The check is contiguity and `ParentHash == Hash(previous)` (`:668-677`).
- `PrimeAncestorBodies` writes bodies, and receipts when given, for the same blocks, so that the reward hook's signing-transaction walk can read blocks and receipts; without them the first post-pivot epoch switch pays nobody and the state root diverges (`downloader.go:714-779`). A body that is a placeholder (no transactions) for a header that declares transactions is overwritten (`:736-748`).
- `SetSnapTerminateFloor(floor)` makes the skeleton's reverse walk stop at the state pivot instead of walking to genesis (`eth/downloader/snap_terminate_floor_xdc.go:51-56`, doc comment `:17-50`). The state pivot is `head - fsMinFullBlocks`; it is not a knob for an external pivot (`docs/port/recreate-guide/README.md` guide 11).
- Snap and heal are switched per chain in `eth/sync_xdc.go:56-89` (`XDC_EXPERIMENTAL_SNAPSYNC`, `XDC_SNAP_HEAL`, the trusted chain ids).

geth's window is a constant two epochs (A.68.2, A.72); the plan is derived, and it is larger on Apothem (`spec/retention.md`, "What geth does about it"). xOne uses the plan.

### What xOne does

`xone-snap` cannot depend on `xone-sync` (same rank in `ci/layering.py`), so `client` has its own `Need { kind, first, last }` and `Kind` (the same five variants as `bootstrap::Class`). The caller maps `Plan::needs` to `Need`s (`conformance/snap/tests/plan.rs` does, for `pivot_plan`), passes the state root of the pivot header, and a `History`:

```
trait History { fn has(&self, kind, number) -> bool;
                fn fetch(&mut self, kind, first, last) -> Result<(), PeerError>; }
finish(store, pivot_state_root, needs, history) -> Result<(), SyncError>
```

`finish` does this:

1. For every range that is not `State`, find the blocks `has` says are missing, as ranges.
2. Ask `fetch` for each missing range. The implementation gets headers, bodies and receipts over eth (`GetBlockHeaders`, `GetBlockBodies`, `GetReceipts`), verifies parent links and body roots against the headers, and stores them; the result is not trusted.
3. Check again with `has`. Everything still missing is `SyncError::Incomplete(Incomplete(ranges))`.
4. `State` is held only when `store.progress.verified == Some(pivot_state_root)`: the downloaded state was rebuilt and its root equals the `stateRoot` of the pivot header. A state of another root, or one that was never checked, is `State` missing.

Nothing weakens: a fetch that returns `Ok` but stores nothing, or stores a wrong range, is caught by the second `has`; a range that cannot be fetched is a refusal with the exact hole.

## Tests

- `conformance/snap/tests/plan.rs`: Apothem `P = 83,600,000`, the plan from `pivot_plan`; with the state verified and nothing below the pivot, `finish` refuses with the six missing ranges and asked for each first; with headers and receipts held and bodies for the last 100 blocks only, it refuses with the body range `P-1798 ..= P-100` and no other; with a source for bodies it completes; with another state root it refuses with `State`.
- `crates/xone-snap/tests/sync.rs`: `completeness_refuses_missing_pre_pivot_data` (a hole in the middle is reported as that hole; the same hole fetched completes).

## Not covered

- The `History` that talks eth and stores headers, bodies and receipts in the database (the node binding, XB-14); only the check and the refusal are here.
- The check that a fetched header chain links to the anchor header and that the epoch-switch header it contains matches the anchor's masternodes (#14, `bootstrap::verify_anchor`).
- Gap snapshots are derived from the 0x88 state at the gap block, which a pivoted node does not have; the plan lists them, `History::fetch(GapSnapshot, ..)` is the hook, and a source for them (re-derivation from a peer's proof, or the snapshot file of XB-18) is not part of this slice.
