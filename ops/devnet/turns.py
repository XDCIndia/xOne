#!/usr/bin/env python3
"""The turn table of one validator, from chain data only (the mint-gate skill: count turns from the chain, not from
the candidate's log). Reads a geth-XDC node over its loopback HTTP RPC.

    turns.py GETH_URL VALIDATOR [FROM [TO|latest]]

For every V2 round from the round of block FROM to the round of block TO, the leader is
`masternodes[round % epoch % len]` in the masternode list of the round's epoch (geth `engine_v2/mining.go:67`;
the list is `XDPoS_getMasternodesByNumber` of a block of that epoch). A round with a block is sealed by its
leader (geth refuses any other); a round with no canonical block was a timeout (or an orphaned proposal) of its
leader. Prints one line per epoch and the totals as JSON on the last line.
"""
import json, sys, urllib.request

url, me = sys.argv[1], sys.argv[2].lower()
lo = int(sys.argv[3]) if len(sys.argv) > 3 else 31
EPOCH = 30


def call(method, params):
    q = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, q, {"content-type": "application/json"})
    r = json.load(urllib.request.urlopen(req, timeout=10))
    if "error" in r:
        raise SystemExit(f"{method}: {r['error']}")
    return r["result"]


hi = sys.argv[4] if len(sys.argv) > 4 else "latest"
hi = int(call("eth_blockNumber", []), 16) if hi == "latest" else int(hi)
blocks = {}  # round -> (number, coinbase)
lists = {}  # epoch -> masternodes
for n in range(lo, hi + 1):
    st = call("XDPoS_getMasternodesByNumber", [hex(n)])
    b = call("eth_getBlockByNumber", [hex(n), False])
    r = st["Round"]
    blocks[r] = (n, b["miner"].lower())
    lists.setdefault(r // EPOCH, [a.lower() for a in st["Masternodes"]])
r0, r1 = min(blocks), max(blocks)
tot = dict(rounds=0, eligible=0, sealed=0, missed=0, missed_rounds=[], timeouts_by_leader={}, epochs=0)
for e in sorted(lists):
    ml = lists[e]
    row = dict(epoch=e, masternodes=len(ml), x_in_list=me in ml, rounds=0, eligible=0, sealed=0, timeouts=0)
    for r in range(max(r0, e * EPOCH), min(r1, e * EPOCH + EPOCH - 1) + 1):
        leader = ml[r % EPOCH % len(ml)]
        row["rounds"] += 1
        hit = blocks.get(r)
        if hit is None:
            row["timeouts"] += 1
            tot["timeouts_by_leader"][leader] = tot["timeouts_by_leader"].get(leader, 0) + 1
        if leader == me:
            row["eligible"] += 1
            if hit is not None and hit[1] == me:
                row["sealed"] += 1
            else:
                tot["missed_rounds"].append(r)
    print(json.dumps(row))
    tot["epochs"] += 1
    for k in ("rounds", "eligible", "sealed"):
        tot[k] += row[k]
tot["missed"] = tot["eligible"] - tot["sealed"]
tot.update(first_block=lo, last_block=hi, first_round=r0, last_round=r1, blocks=hi - lo + 1,
           sealed_by_x=sum(1 for _, m in blocks.values() if m == me))
print(json.dumps(tot))
