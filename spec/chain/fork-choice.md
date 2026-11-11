# XDC fork choice, equal-height siblings and the stale-sibling head

## Rule

geth-XDC has no total-difficulty comparison in the choice at the pin. After an executed block is written (`writeBlockWithState`), `writeBlockAndSetHead` decides in this order:

1. **Side**: the block is V2 (`number > V2.SwitchBlock`), its parent is not the head, and `number <= head number`. The block is stored with its state and the head stays. Every V2 block has difficulty 1, so a same-height sibling never replaces the head, and neither does a lower block. A higher block does.
2. Otherwise, if the parent is not the head, **reorg** first (unwind the old branch, apply the new one), then set the head to the block.
3. Otherwise the block **extends** the head.

Consequences:

- V2, head `X` at height `n`, sibling `S` arrives: `S` is a side block. A child of `S` (height `n+1`) arrives: its parent is not the head and its number is above the head, so the chain reorganises to `S`'s branch and the child becomes the head. The reverse order (head is the stale sibling `S`, the canonical block `X` arrives, then `X`'s child) ends on `X`'s branch the same way. This is xOneGo `d4cdf136` (net2040 619,713), and the Apothem 85,209,326 and 85,214,323 wedge of `474c3b02`/`e8c845bd`.
- V1 (no V2 section, or `number <= SwitchBlock`): every block whose parent is not the head becomes the head, also a sibling of the head and a block below the head. The canonical index above the new head is deleted.
- TD is computed and stored (`writeBlockWithState`, XDC #1645) for the eth handshake. It is never read by the choice. Storing TD belongs to the handshake item (XB-14), not to this page.
- A known block (state present) goes through the same rule (`writeKnownBlock`), without executing it again. A known canonical block at or below the head is ignored.

## Source

- `core/blockchain.go:2414-2424` `keepsV2Head`; `:2426-2465` `writeBlockAndSetHead` (`:2433` the side test, `:2438-2443` reorg, `:2445` head); `:2234-2248` `writeKnownBlock`.
- `core/blockchain.go:2262-2275` TD written, not read; `:2551-2569` known blocks skipped in `insertChain`.
- `core/blockchain.go:1877-1897` `writeHeadBlock` (canonical index entry and head markers); `core/rawdb/accessors_chain.go:54` `WriteCanonicalHash`, `:61` `DeleteCanonicalHash`.
- `core/blockchain_v2_forkchoice_test.go` the geth test of the V2 rule.

## Networks

Apothem (51), mainnet (50), devnet (5551), devnet551 (551), net2040. V2 applies above each network's `V2.SwitchBlock`.

## Rust

`xone_sync::chain::fork_choice::{decide, Facts, Choice}`: a pure function, no state. `Chain::insert` (`chain/insert.rs`, `settle`) calls it once per executed block. `Chain::is_v2` is `number > spec.xdpos.v2.switch_block`.

## Vectors

`crates/xone-sync/oracle/` drives the pinned `core.BlockChain` (`GenerateChainWithGenesis`, `InsertChain`) with block trees and prints, per insertion, the head and the whole canonical chain geth chose (`tests/vectors/fork_choice.json`, 62 scenarios, 247 events). Nothing in Rust re-derives these answers.

- 8 shape scenarios (4 arrival orders, V2 and V1): stale head then canonical then child; canonical head then sibling then child; stale head then canonical; child of the sibling.
- 50 generated sibling pairs (seeded; random mode, fork height, branch lengths, arrival order).
- 4 committed-block floor scenarios (`reorg-and-set-head.md`).
- `tests/fork_choice.rs` builds each tree with synthetic blocks (real state roots from a model) and requires the same head, the same canonical hashes and the same refusals at every event, then compares the flat state root with the head's `state_root`.
- `tests/shapes.rs` runs the 8 shape scenarios, in both modes, on real blocks from the archives: net2040 619,712/619,713 and Apothem 85,209,326/85,209,327 and 85,214,323/85,214,324 (headers, transactions, receipts and, where the state test crate has them, state diffs). The archive serves no non-canonical sibling: `S` is the canonical block with another timestamp (same transactions, same recorded execution), `T` an empty child of `S`. The real bodies and receipts go through `BodyVerifier`.
- Mutants of the rule (adopt an equal-height sibling; never keep a side block; keep a lower V1 block) disagree with the oracle vectors; a store that executes a side block on the head's state is refused by the real vectors (xOneGo `5c7be429`, `e8c845bd`).

## Not covered

- The oracle runs the ethash faker engine with an XDPoS config: it proves the choice and the index, not header verification. Real V2 headers (QCs, seals) of a sibling are the header verifier's input (XB-11) and need the epoch context a node holds.
- geth's `checkpointSyncNoState` and `TrustedSyncCheckpoints` bulk-sync tolerances (`core/block_validator.go:157-185`, `:206-214`) are not reproduced. There is no switch.
