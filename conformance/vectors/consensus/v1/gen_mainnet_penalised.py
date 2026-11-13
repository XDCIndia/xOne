#!/usr/bin/env python3
"""Writes mainnet_penalised.jsonl: mainnet (chain 50) V1 checkpoints with penalties, and the checkpoints
before them whose penalties still count (LimitPenaltyEpoch = 4). Needs pycryptodome and a public RPC:

    python3 gen_mainnet_penalised.py https://rpc.xdcrpc.com > mainnet_penalised.jsonl

Every value is a header field read from the chain; the header is re-encoded and its hash checked
against the RPC's, so a field the RPC dropped fails here and not in the test."""
import json, sys, time, urllib.request
from Crypto.Hash import keccak

EPOCH, LIMIT = 900, 4
# 469,800: the first penalised checkpoint; 470,700..473,400: the four after it, which still drop its penalty;
# 474,300: the penalised address is back; 475,200: a second penalty; 762,300: two penalties;
# 3,880,800 and 3,881,700: consecutive penalised checkpoints (the earlier penalties are carried into the later).
NUMBERS = [469800, 470700, 471600, 472500, 473400, 474300, 475200, 762300, 3880800, 3881700]

def rpc(url, n):
    req = {"jsonrpc": "2.0", "id": 1, "method": "eth_getBlockByNumber", "params": [hex(n), False]}
    r = urllib.request.Request(url, data=json.dumps(req).encode(), headers={"content-type": "application/json", "user-agent": "curl/8"})
    time.sleep(1)
    return json.load(urllib.request.urlopen(r, timeout=60))["result"]

def enc(x):
    if isinstance(x, bytes):
        return x if len(x) == 1 and x[0] < 0x80 else ln(len(x), 0x80) + x
    b = b"".join(enc(i) for i in x)
    return ln(len(b), 0xC0) + b

def ln(n, o):
    if n < 56:
        return bytes([o + n])
    l = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([o + 55 + len(l)]) + l

def q(v):
    v = int(v, 16)
    return v.to_bytes((v.bit_length() + 7) // 8, "big")

def b(v):
    return bytes.fromhex(v[2:])

def header_rlp(r):
    f = [b(r["parentHash"]), b(r["sha3Uncles"]), b(r["miner"]), b(r["stateRoot"]), b(r["transactionsRoot"]),
         b(r["receiptsRoot"]), b(r["logsBloom"]), q(r["difficulty"]), q(r["number"]), q(r["gasLimit"]),
         q(r["gasUsed"]), q(r["timestamp"]), b(r["extraData"]), b(r["mixHash"]), b(r["nonce"]),
         b(r["validators"]), b(r["validator"]), b(r["penalties"])]
    return enc(f)

url = sys.argv[1]
penalties = {}
for n in sorted({k - EPOCH * i for k in NUMBERS for i in range(0, LIMIT + 1)}):
    penalties[n] = rpc(url, n)
for n in NUMBERS:
    r = penalties[n]
    raw = header_rlp(r)
    h = "0x" + keccak.new(digest_bits=256, data=raw).hexdigest()
    assert h == r["hash"], (n, h, r["hash"])
    earlier = [{"number": n - EPOCH * i, "penalties": penalties[n - EPOCH * i]["penalties"]} for i in range(1, LIMIT + 1)]
    print(json.dumps({"number": n, "hash": h, "header": "0x" + raw.hex(), "earlier": earlier}))
