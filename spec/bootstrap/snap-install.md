# Snap sync: the install path

Code: `xone_node::snapshot::install` (shared with `snapshot import`), `xone_sync::store::Store::install_snapshot`,
`xone_node::services::snapsync`. Pivot rule: [pivot](../snap/pivot.md).

A snap sync ends in the same state as `xone snapshot import` (`import.md`): the datadir is anchored, its
content is one generation, and the node starts it as a minimal node. There is one install function for both.

## Inputs, as the import has them

| import (`import.rs`, `snapshot.rs`) | snap sync |
| --- | --- |
| manifest, `verify_anchor` against `--pins` | anchor built by `build_anchor` from served headers, `verify_anchor` against `--snap.pins` or two peers |
| header chunks, linked down from the checkpoint | the header window of `plan_at(P)`, linked |
| bodies and receipts, `BodyVerifier` | the same, from eth requests |
| account and slot rows, state root rebuilt (the file names addresses; the install hashes them and keeps them as preimages) | snap/1 ranges with proofs, hashed rows as they come, state root rebuilt from them |
| side chunk: codes, gap pools, sign transactions (from the exporting node's history) | codes from snap/1 by hash; gap pools from proven 0x88 state at each gap block; sign transactions from the window's bodies and receipts |
| V1 signer snapshot | not needed: snap sync starts on V2 only |

The snap path proves more of the side data than the import does: the import takes the gap pools and sign
transactions of the side chunk from the exporter, the snap path derives each from proven state or from bodies
checked against their headers.

## Write order

1. Nothing is written until every step of the download has passed. A stop or a failure before this leaves the
   datadir empty (`head.json` absent), and the next start begins again.
2. `snapshot::install`: the state checkpoint of the pivot (`ckpt/`, gzip JSON: accounts, storage, codes, gap
   pools, sign transactions, `signs_from` 0, `td` the pivot's from the source's Status (`snap/pivot.md` rule 6;
   0 for a snapshot file, which carries none)), then `Store::install_snapshot`: one generation with the
   state, the canonical index, the headers, the records, the anchor manifest and the segment manifest whose
   `first_block` is the first retained block.
3. `<datadir>/xone/snap-pivot.txt`: the pivot, its state root, the switch header and the pin source.

## After

`--syncmode snap` on an anchored datadir opens it with `minimal`. Below the window the node answers with the
typed `UnsupportedHistory` error, as an imported datadir does (`minimal-mode.md`). The first executed block is
`P + 1`.
