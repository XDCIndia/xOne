# Who builds the next block (XB-23)

## Rule

**V1.** The masternode after the parent's creator in the list of the parent's epoch is in turn. Any other masternode may seal out of turn once `wait` seconds have passed since the parent's timestamp: `10 * hops`, and `20 * hops` when the hops reach the next checkpoint (`hops >= epoch - parent % epoch`). `hops = Hop(len, pre, cur)`. A node outside the list, or a parent whose creator is outside it (except the genesis parent), has no slot. The block's difficulty is `len - hops` (`v1::turn::difficulty`); a node that waited builds a block of the lower difficulty.

**V2.** The leader of round `r` is `masternodes[r % epoch % len]` in the list of the round: the new list when the block opens an epoch (computed, not read), else the parent's. A node that is not in the list, or not the leader, does not propose. A round at or below `highestSelfMinedRound` is never proposed again (`ErrAlreadyMined`). **That round is the `Propose` slot of the safety record** (ADR-010, `Kind::Propose`), written before the seal signature exists, so it survives a restart (xOneGo #624). The unit reports it through `Sealer::signed_up_to(Kind::Propose)`; the schedule asks the unit, not a second copy.

**V1 seals** use their own slot (`Kind::SealV1`, keyed by block number): a V1 height is not a V2 round and must never bound one.

## Source

- V1: `consensus/XDPoS/xdpos.go:1983-2047` `YourTurn`, `:1976-1980` `v1MinePeriod`, `:1875-1881` `calcDifficulty`, `:2658` `Hop`.
- V2: `consensus/XDPoS/engines/engine_v2/mining.go:24-71` `yourturnAligned` (`:26-31` already mined, `:67` formula), `engine.go:137-143` `markSelfMined`, `:587-588` the restored value, `engine.go:869-897` `YourTurn`.

## Rust

`xone_builder::producer::schedule::{v1_slot, v1_may_seal, v2_turn, V1Slot, Skip}`.

## Vectors and mutants

`tests/producer_schedule.rs`: 200 real Apothem V1 blocks (each creator is in a slot the rule allows, difficulty equal, 0 out of turn in that window) and hand vectors of the hop wait; 200 real Apothem V2 blocks (the coinbase leads its round in the list of its epoch, across the first epoch switch; the already-mined and not-leader answers). `tests/producer_sim.rs`: three validators through the XB-15 machine, only the leader proposes. Mutants: mined-round guard off, any masternode may lead (V2), no out-of-turn wait (V1).

## Not covered

The leader-side sibling proposal after a timeout (the leader builds on the highest-QC block, not the head): the producer builds on the block of the highest QC, but only when it is the chain head; the node executes no side branches yet. A validator that joins mid-V2 (xOneGo #718) is covered by the machine's chain facts, not by a devnet run.
