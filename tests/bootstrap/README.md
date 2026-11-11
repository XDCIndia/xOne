# Bootstrap fixtures

Real answers of the Apothem archive (`xdc02`, local JSON-RPC port 21105, geth-XDC `XDC/v1.17.10-xdc.20-08d25caf`, head 85,245,927), fetched read-only on 2026-11-11 by `fetch.py` at most 4 requests a second. Nothing here is edited by hand; run `fetch.py` again. Read by `crates/xone-sync/tests/bootstrap.rs`; every header is checked there against the hash the archive reported.

| file | what | fetched with |
| --- | --- | --- |
| `apothem-v2-84954819.jsonl` | the epoch switch 84,954,819 (95 masternodes, round 28,754,100) and the 15 blocks above it | `fetch.py headers 84954819 84954834` |
| `apothem-masternodes-84954831.json` | `XDPoS_getMasternodesByNumber(84954831)`: the set the archive reports for that epoch | `fetch.py masternodes 84954831` |
| `apothem-v2switch-56828700.jsonl` | the V2 switch block 56,828,700 (a V1 header, 13 masternodes in `extraData`) and the 15 blocks above it | `fetch.py headers 56828700 56828715` |
| `apothem-v1-50000400.jsonl` | the V1 checkpoint 50,000,400 (10 masternodes) and the 17 blocks above it | `fetch.py headers 50000400 50000417` |
| `apothem-epoch-switches-83000425-85245161.json` | the 2,533 V2 epoch-switch block numbers the archive lists between 83,000,000 and the head | `fetch.py switches 83000000 85245927` |

The header files hold the keys of `eth_getBlockByNumber(n, false)` without `transactions`, `uncles`, `withdrawals`, `size` and `totalDifficulty`, sorted, one object a line (the format of `conformance/vectors/headers/`). `apothem-v1-50000400.jsonl` and `apothem-v2-84954819.jsonl` were fetched with a longer range (40 and 25 blocks) and cut to their first lines.
