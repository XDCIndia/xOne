import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import *

def candidates_at(g):
    ln = int(arch.call(P, "eth_getStorageAt", [SMC, "0x8", hex(g)]), 16)
    base = int.from_bytes(k256((8).to_bytes(32, "big")), "big")
    res = arch.batch(P, [("eth_getStorageAt", [SMC, hex(base + i), hex(g)]) for i in range(ln)])
    return ["0x" + r[-40:] for r in res if int(r, 16) != 0]

def upgraded(s):
    return s >= int(NETCFG["blocks"]["tipUpgradePenaltyBlock"])

def build(sw, ks, k):
    s, prev = ks[k], ks[k - 1]
    hist = [sw[str(ks[k - 1 - i])]["pen"] for i in range(5) if k - 1 - i >= 0]
    parent_mn = sw[str(prev)]["val"]
    exp = sw[str(s)]["pen"]
    g = (s - s % 900) - 450
    cands = candidates_at(g)
    hd = arch.hdrs(P, prev, s - 1)
    # bodies: the last 150 blocks before the upgrade, the whole epoch from it
    body_lo = prev if upgraded(s) else max(prev, s - 150)
    blocks = arch.blocks(P, body_lo, s - 1)
    miners = [b["miner"] for b in hd if b["number"] > prev]
    h15 = {b["number"]: b["hash"] for b in hd if b["number"] % 15 == 0}
    inv = {v: n for n, v in h15.items()}
    watch = set(a for l in hist for a in l)
    signs = [[b["number"], x["from"], inv[x["signed"]]] for b in blocks for x in b["signs"] if x["from"] in watch and x["signed"] in inv]
    rnd = arch.call(P, "XDPoS_getMasternodesByNumber", [hex(s - 1)])["Round"]
    table = sorted(set(parent_mn) | set(cands) | set(exp) | watch | set(miners) | {x[1] for x in signs})
    ix = {a: i for i, a in enumerate(table)}
    return {"id": f"{NET}-{s}", "network": NET, "number": s, "prev": prev, "round": rnd, "gap": g, "addrs": table,
            "parent_masternodes": [ix[a] for a in parent_mn], "candidates": [ix[a] for a in cands],
            "history": [[ix[a] for a in l] for l in hist], "miners": [ix[a] for a in miners],
            "hash15": {str(n): h for n, h in h15.items()}, "signs": [[n, ix[a], m] for n, a, m in signs],
            "header_penalties": [ix[a] for a in exp]}

if __name__ == "__main__":
    sw_file, out, *picks = sys.argv[1:]
    sw = {}
    for f in sw_file.split(","):
        sw.update(json.load(open(f)))
    ks = sorted(int(x) for x in sw)
    res = []
    for p in picks:
        res.append(build(sw, ks, ks.index(int(p))))
        print("built", p, flush=True)
    json.dump(res, open(out, "w"), separators=(",", ":"))
