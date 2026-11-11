"""Fetches, from the xdc02 archives (read-only JSON-RPC over ssh, at most 5 requests per second),
the block-level state diff of each named block, for tests/vectors/sibling_diffs.json.

debug_traceBlockByNumber with prestateTracer diffMode returns one {pre, post} per transaction. This
script merges them in order into one block diff: pre = first value seen, post = value after the last
transaction. Block-level effects outside transactions (checkpoint rewards) are not in a trace; the
four blocks are not checkpoint blocks (number % 900 != 0).

The trace does not see the system write each block makes to the EIP-2935 history contract (slot
(number-1) % 8191 receives the parent hash). It is added from eth_getProof at number-1 and number,
and the merged account set is checked against debug_getModifiedAccountsByNumber(number-1, number),
which is the archive's own list of changed accounts. An account on that list that the trace missed
(the transaction-fee owner is credited outside the traced calls) is read with eth_getProof at
number-1 and number; its storage hash must be equal at both, so no storage change is hidden.

Usage: python3 fetch_sibling_diffs.py > ../tests/vectors/sibling_diffs.json
"""
import json
import subprocess
import sys
import time

HOST = "xdc02"
BLOCKS = [("net2040", 10071, 619712), ("net2040", 10071, 619713),
          ("apothem", 21105, 85209326), ("apothem", 21105, 85214323)]
ZERO = "0x" + "00" * 32
EMPTY_CODE = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"


def rpc(port, method, params):
    time.sleep(0.25)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    out = subprocess.run(["ssh", HOST, "curl", "-s", "-m", "120", "-H", "'content-type: application/json'",
                          "-d", "'%s'" % body, "http://127.0.0.1:%d" % port],
                         capture_output=True, text=True, check=True).stdout
    r = json.loads(out)
    if "error" in r:
        raise SystemExit("rpc error %s %s: %s" % (method, params, r["error"]))
    return r["result"]


def pad(h):
    return "0x" + h[2:].rjust(64, "0")


def fresh(a):
    return {"nonce": 0, "balance": "0x0", "code_hash": EMPTY_CODE, "storage": {}}


def apply_post(cur, p, touched):
    if "balance" in p:
        cur["balance"] = p["balance"]
    if "nonce" in p:
        cur["nonce"] = p["nonce"]
    if "codeHash" in p:
        cur["code_hash"] = p["codeHash"]
    for s in touched:
        cur["storage"][s] = pad(p.get("storage", {}).get(s, ZERO))


def merge(traces):
    pre_block, cur, seen_slots = {}, {}, {}
    for t in traces:
        pre, post = t["result"]["pre"], t["result"]["post"]
        for a in set(pre) | set(post):
            if a not in pre_block:
                if a in pre:
                    p = pre[a]
                    pre_block[a] = {"nonce": p.get("nonce", 0), "balance": p.get("balance", "0x0"),
                                    "code_hash": p.get("codeHash", EMPTY_CODE), "storage": {}}
                else:
                    pre_block[a] = None
                cur[a] = json.loads(json.dumps(pre_block[a]))
            ps, qs = pre.get(a, {}).get("storage", {}), post.get(a, {}).get("storage", {})
            touched = set(ps) | set(qs)
            seen = seen_slots.setdefault(a, set())
            for s in touched - seen:
                if pre_block[a] is not None:
                    pre_block[a]["storage"][s] = pad(ps.get(s, ZERO))
            seen |= touched
            if a in post:
                if cur[a] is None:
                    cur[a] = fresh(a)
                apply_post(cur[a], post[a], touched)
            else:  # in pre, not in post: selfdestructed
                cur[a] = None
    return pre_block, cur


HIST = "0x0000f90827f1c53a10cb7a02335b175320002935"
WINDOW = 8191


def proof_account(port, n, addr, slot=None):
    side, hashes = [], []
    for b in (n - 1, n):
        r = rpc(port, "eth_getProof", [addr, [slot] if slot else [], hex(b)])
        hashes.append(r["storageHash"])
        side.append({"nonce": int(r["nonce"], 16), "balance": r["balance"], "code_hash": r["codeHash"],
                     "storage": {slot: pad(r["storageProof"][0]["value"])} if slot else {}})
    assert slot or hashes[0] == hashes[1], "storage changed in an account read without slots"
    return side


def main():
    out = []
    for net, port, n in BLOCKS:
        assert n % 900 != 0
        blk = rpc(port, "eth_getBlockByNumber", [hex(n), False])
        traces = rpc(port, "debug_traceBlockByNumber",
                     [hex(n), {"tracer": "prestateTracer",
                               "tracerConfig": {"diffMode": True, "disableCode": True}}])
        pre, post = merge(traces)
        accts = {}
        for a in pre:
            if pre[a] != post[a]:
                accts[a] = {"pre": pre[a], "post": post[a]}
        out.append({"network": net, "number": n, "hash": blk["hash"], "parent_hash": blk["parentHash"],
                    "state_root": blk["stateRoot"], "tx_count": len(traces), "accounts": accts})
        assert HIST not in accts
        slot = pad(hex((n - 1) % WINDOW))
        pre_h, post_h = proof_account(port, n, HIST, slot)
        assert post_h["storage"][slot] == blk["parentHash"], "EIP-2935 slot must hold the parent hash"
        accts[HIST] = {"pre": pre_h, "post": post_h}
        modified = set(rpc(port, "debug_getModifiedAccountsByNumber", [n - 1, n]))
        assert set(accts) <= modified, (n, sorted(set(accts) - modified))
        for a in sorted(modified - set(accts)):
            pre_a, post_a = proof_account(port, n, a)
            accts[a] = {"pre": pre_a, "post": post_a}
        print("%s %d txs=%d accounts=%d" % (net, n, len(traces), len(accts)), file=sys.stderr)
    json.dump(out, sys.stdout, indent=1, sort_keys=True)
    print()


main()
