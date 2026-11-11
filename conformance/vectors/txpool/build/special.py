"""Special-transaction admission vectors (XB-16): the candidate pool of a recent head of the Apothem and net2040 archives
(0x88 storage words of the gap block, and the three lists of XDPoS_getMasternodesByNumber), and real sign transactions
to 0x89 sent by masternodes and by penalised candidates, rebuilt from the RPC fields (the hash is checked).

  python3 -I special.py apothem ../special-apothem.json    (needs ssh xdc02; read-only; <= 4 requests per second, cached)
  python3 -I special.py net2040 ../special-net2040.json
"""
import sys, os, json
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../reward/build"))
import arch
from Crypto.Hash import keccak

net, out = sys.argv[1], sys.argv[2]
PORT = {"apothem": 21105, "net2040": 10071}[net]
CHAIN = {"apothem": 51, "net2040": 2040}[net]
EPOCH, GAP = 900, 450
SMC = "0x0000000000000000000000000000000000000088"
BS = "0x0000000000000000000000000000000000000089"
SCAN = {"apothem": 120, "net2040": 300}[net]
PER_CLASS = 4


def k256(b):
    return keccak.new(digest_bits=256, data=b).digest()


def pad32(x):
    return x.rjust(32, b"\0")


def hx(b):
    return "0x" + b.hex()


def call(m, p):
    return arch.call(PORT, m, p)


def words_at(block, slots):
    res = []
    for i in range(0, len(slots), 120):
        r = call("eth_getProof", [SMC, [hx(s.to_bytes(32, "big")) for s in slots[i:i + 120]], hex(block)])
        res += [int(sp["value"], 16) for sp in r["storageProof"]]
    return res


def rlp_item(b):
    if len(b) == 1 and b[0] < 0x80:
        return b
    return rlp_len(len(b), 0x80) + b


def rlp_len(n, base):
    if n < 56:
        return bytes([base + n])
    nb = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([base + 55 + len(nb)]) + nb


def rlp_int(v):
    return rlp_item(v.to_bytes((v.bit_length() + 7) // 8, "big") if v else b"")


def raw_legacy(t):
    items = [rlp_int(int(t["nonce"], 16)), rlp_int(int(t["gasPrice"], 16)), rlp_int(int(t["gas"], 16)), rlp_item(bytes.fromhex(t["to"][2:])),
             rlp_int(int(t["value"], 16)), rlp_item(bytes.fromhex(t["input"][2:])), rlp_int(int(t["v"], 16)), rlp_int(int(t["r"], 16)), rlp_int(int(t["s"], 16))]
    body = b"".join(items)
    return rlp_len(len(body), 0xC0) + body


latest = int(call("eth_blockNumber", []), 16)
H = latest - latest % EPOCH - EPOCH + 300  # the middle of an epoch that is over, so a window of blocks sits inside it
G = H - H % EPOCH - GAP
mn = call("XDPoS_getMasternodesByNumber", [hex(H)])
lists = {"masternodes": [a.lower() for a in mn["Masternodes"]], "penalised": [a.lower() for a in (mn.get("Penalty") or [])],
         "standby": [a.lower() for a in (mn.get("Standbynodes") or [])]}

n = words_at(G, [8])[0]
base = int.from_bytes(k256(pad32((8).to_bytes(32, "big"))), "big")
elems = words_at(G, [base + i for i in range(n)])
cands = sorted({"0x" + w.to_bytes(32, "big")[12:].hex() for w in elems if w})
cap_slots = [int.from_bytes(k256(pad32(bytes.fromhex(c[2:])) + (1).to_bytes(32, "big")), "big") + 1 for c in cands]
caps = words_at(G, cap_slots)
words = [[hx((8).to_bytes(32, "big")), hx(n.to_bytes(32, "big"))]]
words += [[hx((base + i).to_bytes(32, "big")), hx(w.to_bytes(32, "big"))] for i, w in enumerate(elems) if w]
words += [[hx(s.to_bytes(32, "big")), hx(w.to_bytes(32, "big"))] for s, w in zip(cap_slots, caps)]

cls_of = {}
for c, a in (("masternode", lists["masternodes"]), ("penalised", lists["penalised"]), ("standby", lists["standby"])):
    for x in a:
        cls_of[x] = c
blocks = arch.batch(PORT, [("eth_getBlockByNumber", [hex(b), True]) for b in range(H - SCAN, H + 1)])
txs, got = [], {"masternode": 0, "penalised": 0, "standby": 0}
for prev, blk in zip(blocks, blocks[1:]):
    for t in blk["transactions"]:
        if (t["to"] or "").lower() != BS or int(t["type"], 16) != 0:
            continue
        c = cls_of.get(t["from"].lower())
        if c is None or got[c] >= PER_CLASS:
            continue
        raw = raw_legacy(t)
        assert hx(k256(raw)) == t["hash"], t["hash"]
        bal = call("eth_getBalance", [t["from"], hex(int(blk["number"], 16) - 1)])
        txs.append({"class": c, "block": int(blk["number"], 16), "hash": t["hash"], "from": t["from"].lower(), "nonce": int(t["nonce"], 16),
                    "balance": str(int(bal, 16)), "head_gas_limit": int(prev["gasLimit"], 16), "raw": hx(raw)})
        got[c] += 1
res = {"network": net, "chain_id": CHAIN, "head": H, "gap": G, "epoch": EPOCH, "words": words, "lists": lists, "sign_txs": txs}
json.dump(res, open(out, "w"), separators=(",", ":"))
print(net, "head", H, "gap", G, {k: len(v) for k, v in lists.items()}, "candidate words", len(cands), "sign txs", got)
