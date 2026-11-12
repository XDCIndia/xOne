#!/usr/bin/env python3
"""The sign transactions (to 0x89) of a chain, from a geth-XDC node's loopback HTTP RPC.

    signs.py GETH_URL XONE_VALIDATOR [FROM [TO|latest]]

For every sign block N (N % 15 == 0) in the range: which masternodes' sign transactions for N are in the chain,
in which block, and whether that block was sealed by the xone validator or by a geth validator. Last line: totals.
"""
import json, sys, urllib.request

url, x = sys.argv[1], sys.argv[2].lower()
lo = int(sys.argv[3]) if len(sys.argv) > 3 else 1


def call(m, p):
    q = json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode()
    r = json.load(urllib.request.urlopen(urllib.request.Request(url, q, {"content-type": "application/json"}), timeout=10))
    return r["result"]


hi = sys.argv[4] if len(sys.argv) > 4 else "latest"
hi = int(call("eth_blockNumber", []), 16) if hi == "latest" else int(hi)
seen = {}  # signed block -> {signer: (in block, sealer)}
for n in range(lo, hi + 1):
    b = call("eth_getBlockByNumber", [hex(n), True])
    sealer = b["miner"].lower()
    for t in b["transactions"]:
        if (t["to"] or "").lower() != "0x0000000000000000000000000000000000000089":
            continue
        signed = int(t["input"][10:74], 16)
        seen.setdefault(signed, {})[t["from"].lower()] = (n, "x" if sealer == x else "geth")
tot = {"sign_blocks": 0, "x_signs": 0, "x_signs_in_geth_blocks": 0, "geth_signs": 0, "geth_signs_in_x_blocks": 0,
       "late": []}
for s in sorted(k for k in seen if lo <= k <= hi):
    row = seen[s]
    tot["sign_blocks"] += 1
    for who, (at, by) in row.items():
        if who == x:
            tot["x_signs"] += 1
            tot["x_signs_in_geth_blocks"] += by == "geth"
        else:
            tot["geth_signs"] += 1
            tot["geth_signs_in_x_blocks"] += by == "x"
        if at != s + 1:
            tot["late"].append([s, who[:8], at])
    print(json.dumps({"signed": s, "signers": len(row), "x_signed": x in row,
                      "in": sorted({a for a, _ in row.values()})}))
print(json.dumps(tot))
