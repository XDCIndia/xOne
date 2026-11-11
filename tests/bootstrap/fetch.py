"""Fetches the fixtures of this directory from an Apothem archive. Read-only, at most 4 requests a
second, through `ssh <host> curl http://127.0.0.1:<port>` (nothing is exposed). Run with `python3 -I`.

    fetch.py headers FIRST LAST OUT     eth_getBlockByNumber(n, false), 5 per request, one request a second
    fetch.py switches FIRST LAST OUT    XDPoS_getEpochNumbersBetween in chunks of 90,000 blocks
    fetch.py masternodes NUMBER OUT     XDPoS_getMasternodesByNumber

Environment: none. The host and port are fixed below. Never edit an output by hand: run this again.
"""
import json
import subprocess
import sys
import time

HOST, PORT = "xdc02", 21105


def call(payload):
    cmd = ["ssh", "-o", "BatchMode=yes", HOST,
           "curl -s -m 120 -H 'content-type: application/json' --data @- http://127.0.0.1:%d" % PORT]
    return json.loads(subprocess.run(cmd, input=json.dumps(payload), capture_output=True, text=True, check=True).stdout)


def rpc(method, params):
    time.sleep(0.3)
    r = call({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    if r.get("result") is None:
        sys.exit("%s%s: %s" % (method, params, r))
    return r["result"]


def headers(first, last, out):
    rows, n = [], first
    while n <= last:
        nums = range(n, min(n + 5, last + 1))
        batch = [{"jsonrpc": "2.0", "id": i, "method": "eth_getBlockByNumber", "params": [hex(b), False]}
                 for i, b in enumerate(nums)]
        rows += [r["result"] for r in sorted(call(batch), key=lambda r: r["id"])]
        n += 5
        time.sleep(1.0)
    with open(out, "w") as f:
        for b in rows:
            for k in ("transactions", "uncles", "withdrawals", "size", "totalDifficulty"):
                b.pop(k, None)
            f.write(json.dumps(b, sort_keys=True, separators=(",", ":")) + "\n")


def switches(first, last, out):
    found, b = [], first
    while b < last:
        e = min(b + 89999, last)
        found += rpc("XDPoS_getEpochNumbersBetween", [hex(b), hex(e)])
        b = e + 1
    json.dump(sorted(set(found)), open(out, "w"), separators=(",", ":"))


def masternodes(number, out):
    r = rpc("XDPoS_getMasternodesByNumber", [hex(number)])
    json.dump({"number": r["Number"], "round": r["Round"], "masternodes": r["Masternodes"], "penalty": r["Penalty"]},
              open(out, "w"), separators=(",", ":"))


if __name__ == "__main__":
    kind, args = sys.argv[1], sys.argv[2:]
    {"headers": lambda: headers(int(args[0]), int(args[1]), args[2]),
     "switches": lambda: switches(int(args[0]), int(args[1]), args[2]),
     "masternodes": lambda: masternodes(int(args[0]), args[1])}[kind]()
