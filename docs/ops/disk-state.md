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

Not in part 1 (memory is still large; numbers in the PR): the trie is still in memory (part 2),
the snap download and its install still hold the whole state (part 3), `Store::state_rows`
(snap/1 serving and `snapshot export`) is refused, so a node on disk state serves no snapshot.

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
