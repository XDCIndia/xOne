# V2 masternodes of an epoch and the leader of a round

## Rule

**Leader.** The leader of a block of round `r` is `masternodes[r % epoch % len(masternodes)]`. `r` is the absolute round; the position inside the list does not restart at the switch block. The list is the one of the epoch the block is in: for the block that opens an epoch it is the list computed for that switch (`calcMasternodes`, see `v2-epoch-switch-and-gap-snapshot.md`), for every other block the list of the parent's epoch, which is the `Validators` of its switch header. A list that is empty has no leader (error).

**A masternode added in the middle of an epoch** (xOneGo #675). The candidate list of the epoch is fixed at its gap block; a candidate that proposes later is in the pool of the next epoch only. So for the rest of the epoch the list and its length do not change, and `r % epoch % len` keeps its phase. At the next switch the list is recomputed from the new gap pool, and from that block the length is the new one. A producer that rereads 0x88 during the epoch, or counts rounds from the switch block, desynchronises from every other node at once, which is what net5151 showed (8 to 9 masternodes at block 57,719, 0 blocks from the xOne node).

**Maximum.** `maxMasternodes` is the config at the round of the block that opens the epoch.

## Source

- `engine_v2/verifyHeader.go:480-481` leader check; `engine_v2/mining.go:24-80` `yourturnAligned` (`:34` switch at round, `:43` recompute, `:49` reuse the parent's list, `:67` formula); `engine_v2/engine.go:2025` `maxMasternodes`.

## Rust

`xone_consensus::v2::masternodes::{leader_index, leader, max_masternodes, calc, first_v2_block}`; `HeaderVerifier` uses `leader`.

## Vectors

- `conformance/vectors/v2/leader-apothem.json`, `leader-net2040.json` (built by `build/leader.py`): 50 consecutive V2 blocks around an epoch switch with the round (from `extraData`), the coinbase and the two masternode lists (`XDPoS_getMasternodesByNumber`); the leader equals the author of every block, across the switch.
- `midadd.json` (oracle command `midadd`): the pinned geth `yourturnAligned`, run for 17 (parent round, round) pairs across an epoch in which 0x88 gains a ninth candidate before the next gap block. The 0x88 words before and after are given; the Rust test builds both pools, computes the new list and checks the leader and the list length of every case, including rounds that skip the first round of the epoch.
- Mutant: the leader index without the `% epoch` step.

## Not covered

- A devnet instance of the net5151 incident is not available; the vector is synthetic, produced by the pinned engine.
