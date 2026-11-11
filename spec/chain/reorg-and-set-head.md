# Reorg by unwind, the committed-block floor, and `set-head`

## Rule

**Reorg.** When fork choice says reorg to block `N` (executed and already stored as a side block):

1. Walk both branches to their common ancestor: bring the longer branch to the height of the other, then step both back until the hashes meet. A missing header is `InvalidOldChain` / `InvalidNewChain`.
2. **Floor.** If the common ancestor is below the latest committed V2 block and the old branch holds that block, refuse with `stop reorg` (typed: `ReorgError::BelowCommitted`). Nothing changes: the block stays a side block, head, index and state stay. A common ancestor equal to the committed block is allowed. The two cases geth only logs as "Impossible reorg" (the old branch has another hash at the committed number; the ancestor is at the committed number with another hash) are `FloorInconsistent` errors here, because the floor is persisted and such a state is a corrupt store.
3. Unwind the old branch to the ancestor in **one atomic step** (ADR-005 section 8, U1-U5: state through the changesets, receipts and segment tails, canonical index, consensus rows, one generation). The unwound blocks stay as side blocks with their state diffs, so the branch can be adopted again.
4. Commit the blocks of the new branch in order, each one a generation (ADR-005 S1-S7). The last one is the new head. A crash between two steps leaves a consistent head (the ancestor or a block of the new branch); `resume.md`.

**Committed floor.** The committed V2 block is the highest block the 3-chain rule committed (`commitBlocks`): when a QC certifies `B` of round `r`, with parent round `r-1` and grandparent round `r-2`, the grandparent is committed; never twice, never lower. The rule is `xone_consensus::v2::commit::commit_blocks`. It is applied to the QC of every block that becomes canonical, and the result is written in the same transaction as the block (`Store::commit_block(rec, committed)`), so it survives a restart. geth keeps it in memory (`highestCommitBlock`) and restarts at nil; the floor here is stricter. A QC whose blocks the store does not hold (a store that starts at an anchor) commits nothing, as geth's `commitBlocks` error does not stop the block.

**`set-head N`.** Rewinds to `N`: state, receipts, canonical index and consensus rows in one atomic step (the same unwind as a reorg). The blocks above `N` are **deleted** (geth `delFn`), so importing them again executes them again and must give the same roots. The consensus caches are then dropped (`InvalidateCaches`). `N == head` is a no-op. Refused with a typed error: `N` above the head; `N` below the committed block; no state retained at `N`.

Differences from geth, all refusals: geth's `SetHead` has no committed floor (the in-memory floor is nil after a restart, and the operator rewinds as far as needed) and rewinds below the target to the nearest block with state; xOne refuses both. A deeper operator rewind than the floor allows needs an owner decision (see the report of XB-13).

## Source

- `core/blockchain.go:3433-3478` `reorg` (branches, common ancestor); `:3479-3516` the committed-block guard (`:3498` "stop reorg", `:3504`, `:3513` "Impossible reorg"); `:3600-3615` the new branch is made the head block by block (`writeHeadBlock`); `:3625-3640` index entries above the new head are deleted (`DeleteCanonicalHash`); `:134-140` `committedBlocker`.
- `core/blockchain.go:3281-3290` the same guard before state recovery of a side chain.
- `consensus/XDPoS/engines/engine_v2/utils.go:476-528` `processQC`, `:530-598` `commitBlocks`; `engine.go:3287` `GetLatestCommittedBlockInfo`; `consensus/XDPoS/xdpos.go:434` `LatestCommittedBlock`.
- `core/blockchain.go:1194-1206` `SetHead` (`:1203` `InvalidateCaches`), `:1436-1560` `setHeadBeyondRoot` (`:1501-1521` `delFn` deletes bodies and receipts); `engine_v2/engine.go:3455` `InvalidateCaches`.
- `core/rawdb/accessors_chain.go:54`, `:61` canonical index write and delete.
- Findings: F-2026-19773 (Hacken F-19773), CertiK XDC-08: a reorg could revert a committed V2 block.

## Networks

V2 networks above their switch block (floor). `set-head` and reorg on all.

## Rust

`xone_sync::chain::reorg::{plan, check_floor, Plan, ReorgError}` (pure), `Chain::adopt` in `chain/insert.rs`, `chain/set_head.rs`, `Store::{unwind_to(n, Keep | Drop), commit_block(rec, committed), committed()}`.

## Vectors

- `tests/vectors/fork_choice.json` `floor/*`: the oracle wraps the engine in the `committedBlocker` interface (`LatestCommittedBlock`) the way geth's own test does. With the committed block set, an alt branch from below it is refused at the block that would become the head (`stop reorg`), from the committed block or above it goes through. The Rust floor is derived from real QC rounds, and its hash equals the label given to the oracle before every scenario.
- `tests/chain.rs::deep_reorg_below_the_committed_block_is_refused`: the malicious deep reorg (alt branch from block 1 below committed block 6, one block past the head): typed error, head, index and state unchanged, the same after a restart.
- `tests/chain.rs::set_head_then_reimport_gives_identical_roots`, `set_head_refusals_and_noop`, `set_head_below_retained_state_is_refused`, `set_head_invalidates_the_consensus_caches`.
- Mutant: a store that forgets the committed block lets the same vector drop block 6 (`planted_bugs_are_caught`); a store whose unwind does nothing cannot adopt the sibling branch.

## Not covered

- Signer safety across a rollback (XA-07).
- `MAX_ATOMIC_UNWIND` (ADR-005 open question 5): the unwind is one transaction of any depth here.
