# Minimal mode: start from the anchor, keep the window, call no archive

Code: `xone_sync::bootstrap::minimal`, `xone_node::services::core::Core::open_with`, flag
`--minimal`. Retention table: `spec/retention.md`.

## Start

`xone --minimal` opens an anchored datadir (one made by `xone snapshot import`) and nothing else:

- a datadir with an anchor, without `--minimal`: `Anchored` (it has no history below its anchor, so
  it cannot be a full node);
- `--minimal` on a datadir without an anchor, or an empty one: `NotAnchored`. A minimal node never
  syncs from genesis and never reads an archive; the only block sources are peers, through the
  same `Chain::insert` as any node (every block is verified and executed);
- `check_start`: the datadir holds, at its head, at least what `Retention::derive(spec, head)` says
  (headers and records from the floor or lower). A datadir that holds less is
  `Shrunk { class, have, need }`: import a new snapshot. Holding more is accepted.

## Prune

After each imported block the node calls `minimal::prune`. The floors for head `h`:
`records_from = min(floor(Body), floor(Receipt), committed)`,
`headers_from = min(floor(Header), records_from)`, where `floor` is `Retention::derive(spec, h)`
(the window of a pivot at `h`) and `committed` is the highest committed V2 block (none on V1). Data
is deleted only when the floor has moved at least `slack` blocks past what is held
(`PRUNE_SLACK` 256 in the node, because the files are rewritten), never at or above the floor, and
never at or above the committed block: a reorg cannot go below it. The manifest `first_block` only
grows; a floor below it is a no-op.

Store: `Store::prune(headers_from, records_from)` (ADR-005 section 10). Headers between the two
floors stay as headers. The node's files are rewritten as a new generation (`headers.<gen>.seg`),
published by `head.json` and deleted behind it; bodies and receipts below `records_from` are empty
frames. The history in memory and the gap pools below `Retention::floor(GapSnapshot)` go too, and
only the newest state checkpoint is kept.

## Serve

`Held` (first block of each class: headers and records from the manifest, gap pools from the
retention floor, state from the head minus the state window) answers
`ChainApi::history(class, block)`. A request below it is
`Missing::UnsupportedHistory { class, block, floor, pivot }`, which the RPC returns as the typed
unsupported-history error; never an empty list, a zero count or a default.

## Test

`crates/xone-node/tests/minimal.rs` and `crates/xone-sync/tests/snapshot.rs`; results in
`docs/ops/snapshot.md`. The follow test uses the committed Apothem V1 vectors (blocks 0..=1800)
because the V2 window vectors need the 294 MB pivot state, which is not in the tree.
