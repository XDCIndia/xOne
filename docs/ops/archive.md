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

