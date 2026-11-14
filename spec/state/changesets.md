# Changesets, flat state and the state of a block: unwind, retention and side branches

## Rule

- A block's changeset holds the whole previous record of every account it touched (absent is recorded as absent) and the previous value of every slot it touched (zero is absent). It never holds a delta. The record includes the wide balance, so nonce, balance and code hash revert together. geth does the same in both places that revert state: the journal stores the previous `Balance` together with the previous `BigBalance`, and the path-database state history stores the previous account as slim RLP, which carries `BigBalance`.
- Deleting an account deletes all of its storage. The changeset then lists every slot the account had, with its previous value, so reverting brings the storage back. If the same block also writes one of those slots, the changeset keeps the value from before the block, not the zero left by the wipe.
- Reverting a block writes its previous values back blindly. Reverting the changesets of blocks `head ... n+1` in that order gives the state, and the root, of block `n`.
- The post-state of a block `n` below the head is the head state with the changesets above `n` read through: for a key, the changeset of the lowest block above `n` that touches it holds the value right after `n`; if none does, the head value is the answer. No replay, no copy.
- A block on a side branch executes on the persisted post-state of its own parent, never on head state. Its writes are a diff on top of the parent view; nothing is written to the head tables for it. A view cannot write: it holds a shared borrow of the provider and has no write method, so a reader cannot change committed state and the provider cannot commit or unwind while a view is open.
- Rewards and the parent-state reads of a block use the state before its first transaction, that is, the parent view (`SetParentState`). The state layer serves it as `at(parent)`; the consumer is XB-08.
- The state diff of a block includes writes outside its transactions. At the pin, the parent hash is written to the EIP-2935 history contract at the start of every block once Prague is active (and the contract is deployed and back-filled at the Prague block), and the transaction fee owner is credited. A trace of the transactions alone does not show them; on the archive, `debug_getModifiedAccountsByNumber` does.
- Retention: the window keeps changesets for the last `retention` blocks. A hash outside it is `Missing`; unwinding below it is `Missing`.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `core/state/journal.go:349-357` `balanceChange` records `previous` and `previousBig`; `:534-537` revert restores both through `core/state/state_object.go:537-541` `setBigBalance`.
- `core/state/stateupdate.go:215-229` account origins are `SlimAccountRLPInto` of the previous account, `nil` when it did not exist.
- `triedb/pathdb/history_state.go:244-250` `stateHistory`: previous account and previous slots per block.
- `triedb/pathdb/states.go:281-323` `revertTo`: origins overwrite the current values blindly; the comment at `:281-288` says a deleted account's original slots stay in the set.
- `core/state/statedb.go:1186-1200` `deleteStorage` collects the original value of every slot of a deleted account.
- `core/state_processor.go:276-277` `SetParentState`: rewards read the pre-transaction state of the block.
- `core/state_processor.go:347-349` and `:589-640` `ProcessParentBlockHash` (EIP-2935 write, with the XDC deploy and back-fill at the Prague block).
- `core/state/statedb_utils.go:16` and `:75` system-contract storage-slot maps, read through this crate by XB-08.
- Storage design this implements: `docs/adr/ADR-005-storage-and-publication.md` section 1 (`AccountChangeSets`, `StorageChangeSets`, "Changesets store whole records, never deltas") and section 8 (atomic unwind).

## Networks

All four.

## Rust

- `xone_state::{StateDiff, ChangeSet}` (`src/changeset.rs`): `StateDiff::apply` returns the changeset; `ChangeSet::revert`. `apply` refuses a balance above `MAX_WIDE_BITS` before any write.
- `xone_state::{FlatRead, FlatWrite, MemFlat}` (`src/flat.rs`): flat tables. They never see a trie.
- `xone_state::{StateProvider, StateView, StateRead, Missing, CommitError}` (`src/provider.rs`): `commit_block`, `unwind_to`, `insert_side`, `at(hash)`, `StateView::child(&diff)`. `StateView` also implements `xone_db::StateProvider` (`account_rlp`, `storage`).
- Tests: `apply_n_then_revert_n_restores_state_and_root`, `every_retained_block_opens_to_its_state_and_unwinds_to_it`, `generated_sibling_pairs_on_one_parent` (1,000 cases), `illegal_balance_is_refused_before_any_write`, `fifty_mutations_unwind_to_every_step_root_and_open_every_block` (unwinds to each oracle root), the provider tests (`outside_the_retained_window_is_a_typed_missing`, `side_branch_stacks_on_a_side_parent_and_head_does_not_move`, `readers_share_a_view_across_threads`, the two `compile_fail` doctests on `StateView`), and the real-data tests in `tests/siblings.rs`.

## Limits of the real-data tests

- The archive does not return a parent trie, so a sibling's canonical `stateRoot` cannot be reproduced from a diff of touched accounts. `tests/siblings.rs` checks the weaker property that the data allows: the canonical diff applied on the parent's touched-account subset leaves the parent unchanged when it is opened again, and two views over one parent are independent. It checks no canonical root. The test names say so.
- `storage_root` is not in a trace; those records keep the default and are compared with the same records on both sides.
- net2040 619,713 is the child of 619,712, and Apothem 85,214,323 is far from 85,209,326. The two blocks of a pair are applied over one merged parent subset (the first block's values win where both touch a key). They are not two blocks with one real parent hash.
- Fixtures come from `crates/xone-state/tools/fetch_sibling_diffs.py`, which merges the per-transaction prestateTracer diffs of a block, adds the EIP-2935 write and the fee owner from `eth_getProof` at `n-1` and `n`, and fails unless its account set equals `debug_getModifiedAccountsByNumber(n-1, n)`.

## Legacy

- A diff cannot express "delete the storage of an account, re-create the account and write new slots" as one account entry: `None` wipes, and slots under a `None` account are ignored. A net diff of such a block lists the surviving slots under a `Some` account and zero for each slot that is gone. The executor (XB-06) builds net diffs.
- Not in this slice: an index per key so a read does not scan the window, a persisted side index, side-branch pruning, and the MDBX binding of the flat tables and changesets.

## Archive history (`--gcmode archive`, issue #152)

- An archive datadir writes each changeset row twice in the block's MDBX transaction: by block (`AccountChangeSets`, `StorageChangeSets`, pruned to the state window) and by state key (`AccountHistory`: `keccak(address) || block`, `StorageHistory`: `keccak(address) || keccak(slot) || block`, never pruned). The value is the same prior record.
- The state after block `n`: for each key, the first history row above `n`, else the head's row (`HashedAccounts`, `HashedStorages`). One cursor seek per read, in one read transaction (`xone_sync::mdbx::history`).
- An unwind deletes the history rows of every block it removes, in the same transaction as the changeset rows (`revert_block`).
- The mode is the `Meta` row `gcmode`, written on an empty datadir only; a start in the other mode is refused (`CoreError::GcMode`). An archive is never installed from a snapshot.
- Tests: `disk_flat_equals_mem_flat` (the history at every block of random blocks, unwinds and restarts equals the memory state, its recomputed root equals the state root, and the row count equals the changesets of the blocks kept) and `crates/xone-node/tests/archive.rs` (net2040 0..=899 with a 64-block window).
