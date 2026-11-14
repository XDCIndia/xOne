# Archive node (`--gcmode archive`)

An archive node keeps the state of every block from genesis. `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`, `eth_getStorageAt` and `eth_call` answer at any canonical block; a full node answers inside its state window and refuses older blocks with the typed `unsupported history` error.

## Start

```
xone --network net2040 --datadir <empty dir> --gcmode archive --syncmode full ...
```

- Accepted on an empty datadir only. The datadir keeps the mode (`Meta` row `gcmode`): a datadir synced with `full` refuses `--gcmode archive`, and an archive refuses `--gcmode full`. The error names the stored mode.
- Full sync only: `--gcmode archive` with `--syncmode snap`, `fast` or `--minimal` is refused at start (an archive has no history below a snap pivot or a snapshot anchor).
- ethstats reports `gcMode: archive`.

## How it is stored

Each block commit writes the block's changeset into `AccountHistory` and `StorageHistory`, keyed by state key then block, in the same MDBX transaction as the block; an unwind deletes them in its transaction. A read at block `n` is one cursor seek per key over the head's tables (`spec/state/changesets.md`, "Archive history").

Not served from the history yet: `eth_getProof` below the window, snap/1 at old roots, and the tracers.

## Growth

Measured on net2040 (`xone-rs-net2040-archive-h95`, full sync from genesis to 832,598 on 2026-11-14, build `fa3edb8e`), sampled every 5 minutes:

| range | `chain.mdbx` (state, changesets, history, indexes) | whole datadir (with the header, body and receipt segments) |
|---|---|---|
| 0 to 215k (blocks of 20,000+ transactions up to ~50k) | 4.56 GB | 20.3 GB |
| 215k to 793k (mostly empty blocks) | +0.47 GB, about 81 MB per 100k blocks | +2.38 GB, about 410 MB per 100k blocks |
| at 832,598 | 5.03 GB | 22.8 GB |

For comparison the full node on the same host (`xone-rs-net2040-h95`, same chain) holds a 2.9 GB `chain.mdbx` and a 20.6 GB datadir: on net2040 the history costs about 2.1 GB, almost all of it from the dense range. MDBX grows in 64 MB steps, so a figure per 100k blocks is rounded to that.
