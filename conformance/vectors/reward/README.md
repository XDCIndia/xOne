# Reward vectors (XB-10)

Checkpoint reward maps of the Apothem archive (`xdc02:21105`, read-only, at most 4 requests per second, export date 2026-11-11, geth-XDC at `conformance/PIN`), the net2040 archive where it was reachable, and made-up windows. Read by `crates/xone-consensus/tests/reward_vectors.rs`. Never edit a vector by hand: run `build/` again.

| file | what |
| --- | --- |
| `apothem.json` | Apothem checkpoints: 83,600,000 and 84,733,000 with two checkpoints on each side, the V1 to V2 switch zone, V1 and V2 checkpoints elsewhere |
| `net2040.json` | net2040 checkpoints around its own `tipFixedRewardBlock` (see below) |
| `synthetic.json` | five made-up windows with voters, a foundation owner, a shared owner and all three tiers |
| `tier_reward.json` | `floor(reward * 1e18)` as `big.Float` computes it, for config rewards and edge values |
| `build/` | the scripts that wrote the files above (Python 3, `pycryptodome`, `eth_keys`; Go for the oracle) |

## Record (`apothem.json`, `net2040.json`, `synthetic.json`)

- `id`, `network`, `kind` (`v1`, `v2`, `v2edge` window crossing the V1 checkpoint of the switch, `tiered`), `number` of the checkpoint block, `round` of its header.
- `start`, `end`: the rewarded blocks. `parent_root`: the state root of block `number - 1`, which the owners were read at.
- `nodes`: every paid node, `owners`: `validatorsState[node].owner` at block `number - 1` (`eth_getStorageAt` on 0x88). `masternodes`: indices into `nodes`. `pool`: the standby pool of the opening epoch (`XDPoS_getMasternodesByNumber`: protectors, observers, standby, in that order), the engine splits it by the V2 config of the round.
- `signed`: for each rewarded block that is a multiple of 15, its hash and the indices of the nodes that sent a sign transaction for it anywhere in the window. The other blocks of the window are not read by the count, so their hashes are not stored; the test fills them in.
- `archive`: what the archive shows for every owner and the foundation: the balance at `number` minus the balance at `number - 1`, minus the effect of the block's transactions on that account (`debug_traceBlockByNumber`, `prestateTracer`, `diffMode`), minus the fee credit of the owner of the author (`spec/evm/fee-recipient.md`: `gasUsed * effectiveGasPrice` of the non-special transactions). `fee` holds the sum and the account taken for the author's owner. A synthetic vector has no archive.
- `oracle`: the map that the pinned geth-XDC reward functions return on the same counts and owners (`crates/xone-consensus/oracle/reward`, `run.sh`). The counts for the oracle are computed by `build/assemble_reward.py`, a third implementation of the count; geth's window walk is checked against the archive balances instead.
- `voters` (synthetic only): the voters of each node with stake. The archive vectors carry no voters, so their maps hold owners and the foundation; voter entries (amount 0) are checked by `synthetic.json`.

None of the checkpoint blocks of the vectors carries a non-special transaction (none of the 1,796 Apothem epoch-switch blocks between 83.6M and 85.2M has any transaction), so the fee term is 0 in every vector. The standby pool is empty at every vector and at all 150 epoch switches sampled between 83.6M and 85.2M (every 12th), so on the archive the protector and observer tiers are paid nobody; those tiers are checked by `synthetic.json` against the oracle only.

The test requires the engine map to equal `oracle` exactly and to equal `archive` for every account in it.

## Regenerating

```
cd conformance/vectors/reward/build
python3 -I run_all.py                       # raw/<number>.json from the archive (cached in cache.db)
python3 -I synth.py
GETH_SRC=<geth-XDC checkout at oracle/PIN> python3 -I assemble_reward.py apothem raw ../apothem.json
```

The penalty vectors (`../penalty/`) come from `build/scan.py` (the `Penalties` and `validators` of every epoch-switch header of a range), `build/build_pen.py` (V2) and `build/build_pen_v1.py` (V1, with `build/v1seal.py` for the sealer and `build/v1scan.py` to find checkpoints with a penalty). net2040: `XB10_NET=net2040 XB10_PORT=10071`; its blocks hold thousands of transactions, so `arch.py` reads its sign transactions through `eth_getLogs` on 0x89.
