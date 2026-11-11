"""Fetches, from the xdc02 archives (read-only JSON-RPC over ssh, at most 5 requests per second), the
canonical blocks of the three sibling shapes of issue #33: net2040 619,712/619,713, Apothem
85,209,326/85,209,327 and Apothem 85,214,323/85,214,324. For each shape: the raw header of the parent
(the anchor), and for the canonical block X and its child Y the raw header, the consensus-encoded
transactions and receipts, and the block-level state diff of X and Y where
`../../xone-state/tests/vectors/sibling_diffs.json` has one (see that crate's tools/fetch_sibling_diffs.py
for how it was made). The archive serves no non-canonical sibling; the sibling is built by the tests.

Usage: python3 fetch_shapes.py > ../tests/vectors/shapes.json
"""
import json
import os
import subprocess
import time

HOST = "xdc02"
SHAPES = [("net2040-619712", "net2040", 10071, 619712),
          ("apothem-85209326", "apothem", 21105, 85209326),
          ("apothem-85214323", "apothem", 21105, 85214323)]
here = os.path.dirname(os.path.abspath(__file__))
DIFFS = json.load(open(os.path.join(here, "../../xone-state/tests/vectors/sibling_diffs.json")))


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


def item(b, o):
    """(is_list, payload start, payload end, item end) of the RLP item at offset o."""
    p = b[o]
    if p < 0x80:
        return False, o, o + 1, o + 1
    if p < 0xB8:
        return False, o + 1, o + 1 + p - 0x80, o + 1 + p - 0x80
    if p < 0xC0:
        n = p - 0xB7
        l = int.from_bytes(b[o + 1:o + 1 + n], "big")
        return False, o + 1 + n, o + 1 + n + l, o + 1 + n + l
    if p < 0xF8:
        return True, o + 1, o + 1 + p - 0xC0, o + 1 + p - 0xC0
    n = p - 0xF7
    l = int.from_bytes(b[o + 1:o + 1 + n], "big")
    return True, o + 1 + n, o + 1 + n + l, o + 1 + n + l


def items(b, start, end):
    out, o = [], start
    while o < end:
        lst, ps, pe, ie = item(b, o)
        out.append((lst, ps, pe, ie, o))
        o = ie
    return out


def split_block(raw_hex):
    b = bytes.fromhex(raw_hex[2:])
    _, ps, pe, _ = item(b, 0)
    top = items(b, ps, pe)
    header = b[top[0][4]:top[0][3]]
    txs = []
    for lst, ps2, pe2, ie2, o2 in items(b, top[1][1], top[1][2]):
        # a legacy tx is a list (the item itself); a typed tx is a byte string (its payload)
        txs.append("0x" + (b[o2:ie2] if lst else b[ps2:pe2]).hex())
    if top[2][2] != top[2][1] or len(top) > 4 or (len(top) == 4 and top[3][2] != top[3][1]):
        raise SystemExit("block has uncles or a non-empty withdrawal list")
    # the block RLP of a post-Shanghai header carries the (empty) withdrawal list as a 4th item
    return "0x" + header.hex(), txs, len(top) == 4


def block(port, n):
    raw = rpc(port, "debug_getRawBlock", [hex(n)])
    header, txs, withdrawals = split_block(raw)
    return {"number": n, "hash": rpc(port, "eth_getBlockByNumber", [hex(n), False])["hash"], "header": header,
            "txs": txs, "withdrawals": withdrawals, "receipts": rpc(port, "debug_getRawReceipts", [hex(n)])}


def diff(network, n):
    for d in DIFFS:
        if d["network"] == network and d["number"] == n:
            return d["accounts"]
    return None


out = []
for name, network, port, x in SHAPES:
    out.append({"name": name, "network": network, "anchor": rpc(port, "debug_getRawHeader", [hex(x - 1)]),
                "x": block(port, x), "y": block(port, x + 1),
                "x_diff": diff(network, x), "y_diff": diff(network, x + 1)})
print(json.dumps(out, indent=1))
