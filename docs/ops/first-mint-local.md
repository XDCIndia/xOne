# First mint, local (XB-23, issue #43)

What this proves: an xone producer seals blocks that a geth-XDC follower imports, on a private two-node loopback devnet, from V1 into V2, with the sign transactions geth expects, and a `kill -9` of the producer neither double-signs nor skips its next turn. **No public network, no real key**: the validator key is a throwaway made by `ops/devnet/gen.py` (`cast wallet new`), and `--mine` is refused on chain ids 50 and 51 in this build (typed refusal `ProducerError::PublicNetwork`, temporary until Gate D). Gate E (a borrowed seat on net2040, then Apothem) needs the owner's mint approval and is not run here.

## Setup

- `xone` built from this branch; `geth-xdc` 1.17.10-xdc.21 built from the pin tree (`conformance/PIN`, `6fc8e8c2`).
- `ops/devnet/gen.py` makes the chain: the pinned devnet genesis with `epoch 30, gap 10`, V2 switch block 30 (`SwitchEpoch 1`), **one masternode** (the throwaway key) in the V1 checkpoint list and as the only candidate of the 0x88 contract. It also writes `forks.json`, the fork table for `xone --chainspec` (the node had only built-in networks; `--chainspec` takes the table of a private chain, refused for ids 50 and 51). The genesis hash is the same on both clients.
- Why the first epoch: the V1 double validation (M2) is assigned from checkpoint headers that are multiples of 900 (`double_validation.go`), so a short-epoch V1 chain cannot run past its first epoch; the devnet switches to V2 there.
- xone: `xone --datadir D --chainspec forks.json --override.genesis genesis.json --networkid 5551 --mine --keystore keystore.json --password password --port 30311 --nodiscover --nat extip:127.0.0.1`. geth: `--syncmode full --nodiscover --netrestrict 127.0.0.0/8`, `admin.addPeer(xone enode)` over IPC.
- `ops/devnet/prove.sh WORKDIR XONE GETH` runs it all and prints the transcript below.

## Transcript (prove.sh, 2026-11-12)

```
gen: {"address": "0xaa785fbef816aa01a9ad9c1d2090816f29782d7c", "genesis": "0x5d7702511b5c53ac67ff31c8079fdc66775e4d8438a02b1a0641dbc124600d7b", "stateRoot": "0xfa71e55dc6cb69abad36081cf952d4e5903da945
[00:35:53] start xone producer
[00:35:59] geth admin.addPeer(xone) -> true
[00:36:16] head 12 (V1)
[00:36:18] kill -9 xone (pid 3059021) in a V1 slot
[00:36:19] restarted xone (pid 3063294)
[00:37:07] head 38 (V2)
[00:37:09] kill -9 xone (pid 3063294) in a V2 round
[00:37:10] restarted xone (pid 3080871)
[00:37:54] geth head 59
--- xone producer log (lines of interest)
producer: validator 0xaA785fbEf816AA01A9Ad9C1d2090816f29782d7c, safety record at round 0
producer: validator 0xaA785fbEf816AA01A9Ad9C1d2090816f29782d7c, safety record at round 0
producer: finishing the block 14 that was being signed
producer: sign transaction 0xd9a8bee1e22bbe8c43381a28deea72d727d7739981e2b845c4a89db021c5fe40 for block 15
producer: sign transaction 0x29fa69c28124cd437b96ba35e113c7d2d2c422303a03972f3603b548b000d17d for block 30
producer: validator 0xaA785fbEf816AA01A9Ad9C1d2090816f29782d7c, safety record at round 9
producer: sign transaction 0x0e5b99479af64d0fa0791a672c677a5ab6edc184f76806963684d8eb49dfe4c7 for block 45
producer: block 60 (round 30) not produced: reward: no second epoch switch below block 60
--- every block number xone sealed, with the number of times: (must all be 1, or 2 when a restart finished a pending block)
59 heights sealed 1 time(s)
--- geth: head, V2 rounds, sign transactions in the chain
"block 16 to=0x..89 nonce=0 gas=200000 gasPrice=0\nblock 31 to=0x..89 nonce=1 gas=200000 gasPrice=0\nblock 46 to=0x..89 nonce=2 gas=200000 gasPrice=0"
"geth head=59 0x1dc256fddd924bb0fb35cfbaf6d75c989951d578a2a8e6096df63b742c9945bf"
--- xone head
producer: block 59 0x1dc256fddd924bb0fb35cfbaf6d75c989951d578a2a8e6096df63b742c9945bf sealed and sent
--- geth warnings about invalid blocks / bad blocks:
0
```

Reading it:

- 59 blocks sealed by xone, each height once, geth head 59 equal to xone's last sealed block hash; geth logged 0 invalid or bad blocks. Blocks 1 to 30 are V1 (block 30 is the V1 checkpoint and the switch block), 31 to 59 are V2 (rounds 1 to 29), each committed by the 3-chain rule one round later, by the node's own BFT machine: the producer's vote goes through the same safety unit.
- Sign transactions: blocks 15, 30 and 45 are sign blocks; the transactions are in geth's blocks 16, 31 and 46 (to 0x89, gas 200,000, gas price 0, nonces 0, 1, 2).
- **kill -9 in a V1 slot** (head 12, 0.9 s after block 13): the restart found `pending.rlp` (block 14 was written to disk, its seal decided and persisted, not yet imported), signed the same bytes again (the unit returns the same signature for the same intent), imported it and sent it: "finishing the block 14 that was being signed". No second block 14; the next turn (block 15) was on time.
- **kill -9 in a V2 round** (head 38): the record came back at round 9 and the producer continued at the next round. This kill landed between blocks, not in the short window between seal and import (milliseconds: a V2 block needs no wait), so the V2 pending path is covered by the same code and by the unit tests (`crates/xone-builder/tests/producer_unit.rs`: a restart keeps the mined round; a retry signs the same bytes), not by this transcript.
- The chain stops at block 59: the first V2 reward is at the second V2 epoch switch (block 60) and its window needs two earlier epoch switches (`reward::v2::locate`, geth `engine_v2_hooks.go:681-728`), which a chain with a single V1 checkpoint does not have. The producer logs `block 60 (round 30) not produced: reward: no second epoch switch below block 60` and does not seal an invalid block. On a chain with two V1 checkpoints before the switch this does not arise; with a short epoch that cannot be made here (see Why the first epoch).

## What is not covered

- The 72 h soak of Gate D, 4 to 14 validators, partitions: not run. One validator, so the quiet window of ADR-010 section 8 is 0 here (there is no second signer); a public network is refused.
- Producer tick budget with a cold state cache, the leader-side sibling proposal after a timeout, a validator that joins mid-V2: not tested on a devnet.
- net2040 borrowed seat and Apothem: Gate E, owner approval needed.
- V1 checkpoint M2 tokens for more than one masternode, and the V1 penalty list before TIPSigning: not built.
