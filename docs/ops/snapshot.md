# Snapshots and minimal nodes (operators)

A snapshot lets a new node start from the state of a recent block instead of from genesis, and a
minimal node keeps only the data the consensus rules still read. Rules: `spec/bootstrap/`.

## Make a snapshot

```text
xone snapshot export --datadir D --out snap.xsnap [--at N] [--max-age SECONDS] [--network apothem]
```

The node must be stopped (the command opens the datadir like `xone start` does). `--at` is the block;
the default is the latest committed V2 epoch switch at or below the head (V1 chains: the highest block
with a seal proof). The state of `--at` must still be inside the node's state window (1,024 blocks
below its head). The file is written to `snap.xsnap.part` and renamed when complete, so a file with
its final name is whole. The same state gives the same bytes: you can publish it under its hash.

## Trust: the pins file

A hash of the file proves its bytes, not that it is the XDC chain. The importer therefore needs one
fact from you, in a pins file, one line per pin (`#` starts a comment):

```text
# Apothem, epoch switch from a source you trust (your own node, a release note, an explorer)
85000100 0x<block hash>
```

A pin is the number and hash of the checkpoint of the snapshot or of the epoch-switch header that
lists its validators (`xone snapshot import` prints neither; read them from a node you trust). The
import is refused without `--pins`, and refused if no pin matches.

## Import

```text
xone snapshot import --datadir D2 --in snap.xsnap --pins pins.txt [--max-age-cap SECONDS] [--network apothem]
```

`D2` must hold no chain. The manifest is checked first (chain, genesis, root, age, certificates or
seals, pin); a snapshot that fails is refused and nothing is written. `--max-age-cap` (default 7 days)
is your limit on how old a manifest may be, whatever it says about itself.

If the command is interrupted, or the file is cut (an unfinished download), run it again with the
same arguments: whole chunks that were checked are kept in `D2/xone/snapshot.part` and the read
continues after them. `Truncated`/`Incomplete` errors name the byte where the file ends. A snapshot
whose window is shorter than this binary needs is refused (`ShortWindow`).

## Run as a minimal node

```text
xone --datadir D2 --apothem --minimal
```

`--minimal` is required for an imported datadir and refused for any other: a minimal node never
syncs from genesis and never calls an archive; its only block source is its peers, and it verifies
and executes every block. It deletes headers, bodies and receipts below the derived window as the head
moves (the files are rewritten every 256 blocks) and keeps one state checkpoint. A request for data
below the window gets the unsupported-history error (`-32000 unsupported history: ...`), never an
empty answer. If a newer binary needs a longer window than the datadir holds, it refuses to start
(`Shrunk`): import a new snapshot.

The node today follows V1 blocks only (the executor of the node service has no V2 rewards yet); a
snapshot at a V2 pivot can be exported, imported and verified, but not followed by `xone start`.

## Measured (not gates)

| what | size | time | where |
| --- | --- | --- | --- |
| snapshot at block 100 of Apothem (real V1 vectors), 20 accounts, 93 slots, 4 codes, 101 headers, 101 bodies | 40,861 bytes, 9 chunks | export 0.13 s, import 0.37 s | release build, `crates/xone-node/tests/minimal.rs` |
| synthetic pivot 85,000,101 (Apothem rules: 4,500 headers, 1,799 bodies, 4 gap pools), 60 accounts, 180 slots | 428,097 bytes, 276 chunks (8 KiB chunks) | export 0.30 s, import 0.87 s | debug build, `crates/xone-sync/tests/snapshot.rs` |
| follow 101..=1800 from one fake peer, minimal node | | 184.6 s (two tests running at once, debug-optimised dependencies; the net service, not the snapshot, sets this) | `tests/minimal.rs` |

Not measured: the Apothem import in 15 minutes and the datadir size after 7 days (issue #38 targets);
both need a public host and a published snapshot.
