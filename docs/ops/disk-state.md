# Disk state (`--state.disk`)

By default xone keeps the head state in memory: every row of `HashedAccounts` and
`HashedStorages` is loaded into a `MemFlat` at start, and every trie node is in memory. That is
fine for net2040 and too much for Apothem (327,356 accounts, about 158 M slots: over 40 GB).
`--state.disk` is the first of three steps to bound that (plan: issue #145).

## Part 1 (this flag)

- `DiskFlat` (`crates/xone-sync/src/mdbx/diskflat.rs`) answers `account` and `storage` from the
  MDBX tables behind two caches (100,000 accounts and 500,000 slots per generation, so at most
  twice that), and scans the tables in pages of 20,000 rows.
- The store writes the tables in the block's own transaction, as before. After the commit the
  block's diff (and, for a deleted account, the slots of its changeset) is written through to the
  caches. An unwind reverts the tables first and then the caches. A cache entry is always what the
  table holds.
- A start does not load rows or replay blocks: the changesets of the retained window are read
  from the tables, the trie is built by one paged scan and its root is checked against the head
  header.
- A checkpoint (`state/ckpt-*.json.gz`) carries no state rows. The snap install does not write a
  state checkpoint either. A datadir of the memory state starts with the flag (the checkpoint rows are ignored); a datadir
  written with the flag must be started with it (its checkpoints have no rows; the node says so).
- A read that fails (a damaged page) sets a flag that stops the store before the next commit.

`Store::state_rows` (snap/1 serving and `snapshot export`) is refused, so a node on disk
state serves no snapshot.

## Part 2: trie nodes on disk

With `--state.disk` the account trie and the storage tries are not in memory either.

- Table `TrieNodes`: a node whose RLP is 32 bytes or more is a row, keyed by position (geth's pathdb
  layout, one byte per nibble): `0 || nibbles` for the account trie, `1 || keccak(address) || nibbles`
  for a storage trie. A node below 32 bytes is inside its parent's row.
- A child that is on disk is a stub (its hash) until a walk loads it. The row must hash to the stub's
  reference, else the trie is marked failed and the store stops before the next commit.
- The store writes the rows a block changed, the rows of the nodes that went away and the rows of
  the storage tries of deleted accounts in the block's own transaction, with the `Meta` row
  `trieroot` (the state root those rows are). An unwind writes the rows of the older state in its own
  transaction. After the commit, clean nodes at depth 5 or below go back to stubs and the storage
  tries are dropped; the account trie keeps its top levels.
- A start uses the rows as they are when `trieroot` is the head header's state root; otherwise
  (a datadir from part 1, a damaged row after a manual edit) it clears `TrieNodes`, rebuilds it from
  the flat tables in one streamed pass (written and pruned every 100,000 accounts and every 200,000
  slots of one account), and checks the root against the head header before it writes the marker.
- Tests: `tests/persisted.rs` (xone-state, proptest): the persisted trie against the in-memory trie
  and `flat_root` over random writes, deletes, unwinds and restarts, with every node but the root a
  stub after each flush, and the row set equal after every step to that of a trie built from the
  final state in one go (no stale row, none missing); a damaged row is refused. `trie_rows.rs`
  (store): rows after blocks and an unwind equal a rebuild's; a missing or wrong marker or a damaged
  row is rebuilt.


## Check

`XONE_DISK_STATE=1 cargo test -p xone-fault -p xone-sync -p xone-state` runs the store and fault
suites over the disk state. The unit test `disk_flat_equals_mem_flat` drives a `DiskFlat` like the
store does (rows first, then the cache) against a `MemFlat` over random blocks, unwinds and
restarts, with caches of one entry and of many.

## Numbers (part 1)

Synthetic state through the real store (`xone-state-bench`, retention 8, 5,000 accounts and 20,000
slots per block, the trie still in memory). Anonymous memory after the run: 500,000 accounts and
2 M slots, memory state 1,329 MB, disk state 1,157 MB (13% less: the flat rows only; the caches
hold what the run wrote). Part 2 (trie nodes) and part 3 (snap download) are what bound memory.

Start-up on net2040 (4,096 blocks of changesets): the state is loaded in 0.2 to 0.6 s with the
disk state at blocks 20,888 and 31,996, and in 21 s with the memory state at block 46,075 (which
holds the rows, and rebuilds the trie from them).

## Part 3: the snap download into the tables

With `--syncmode snap --state.disk` the download holds no state in memory.

- `xone_snap::client::sync` works over a `Backend` (the in-memory `StateStore` as before, or
  `xone_sync::mdbx::snapstate::SnapState`). `SnapState` writes every proven range straight into
  `HashedAccounts`, `HashedStorages` and `Bytecodes` of the store's own `chain.mdbx`, in one
  transaction with the cursor it belongs to (`SnapProgress`: the pivot root, the account cursor,
  the storage cursor of each account). The root of the held rows (for the heal and the final check)
  is streamed from the tables (`state_root_sorted`), a heal walk holds the accounts (they are
  hashed into a trie there) and the slots of one account whose storage trie differs.
- A run that dies resumes: `SnapState::open` reads the cursors, a stored range is not asked for
  again, and a pivot that differs from the stored one goes through `set_root` as a pivot that
  moved does (what came from the old root stays; the heal pass repairs the rest).
- The install is not a copy: `Install::rows_in_tables` says the rows are where they belong.
  `install_snapshot` streams their root and compares it with the pivot header, writes the headers,
  the bodies, the manifest and the anchor in one transaction (and empties `SnapProgress`), then
  builds the trie rows from the tables in one streamed pass (part 2). The checkpoint of the pivot
  carries no state rows.
- A datadir with rows of an unfinished download is not a new datadir: opening it as one
  (`init_anchor`) is refused, with the way out (resume with `--syncmode snap --state.disk`).
- Tests: `tests/snapstate.rs` (store): the same download as the memory store row for row, a kill at
  several requests resumed from the tables (no finished range asked for again), a pivot that moves.
  `state_root_sorted` and `storage_root_sorted` equal `root_of_sorted_rows` and the row roots.
