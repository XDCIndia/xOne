import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import *
from build_reward import v1_masternodes

def pens(n):
    return addrs(header(n)["penalties"]) if n > 0 else []

def build(N):
    signers = [a.lower() for a in arch.call(P, "XDPoS_getSnapshot", [hex(N - 1)])["signers"]]
    prev_mn = v1_masternodes(header(N - 900))
    nxt = v1_masternodes(header(N))
    hp = pens(N)
    cb = pens(N - 4500) if N > 4500 else None
    prior = [a for i in range(1, 5) if N > 900 * i for a in pens(N - 900 * i)]
    hd = arch.hdrs(P, N - 900, N - 1)
    miners = [h["sealer"] for h in hd if h["number"] > N - 900]
    blocks = arch.blocks(P, N - 150, N - 1)
    h15 = {h["number"]: h["hash"] for h in hd if h["number"] % 15 == 0}
    inv = {v: n for n, v in h15.items()}
    watch = set(cb or [])
    signs = [[b["number"], x["from"], inv[x["signed"]]] for b in blocks for x in b["signs"] if x["from"] in watch and x["signed"] in inv]
    table = sorted(set(signers) | set(prev_mn) | set(nxt) | set(cb or []) | set(prior) | set(hp) | set(miners) | {x[1] for x in signs})
    ix = {a: i for i, a in enumerate(table)}
    return {"id": f"{NET}-v1-{N}", "network": NET, "number": N, "addrs": table, "signers": [ix[a] for a in signers],
            "prev_masternodes": [ix[a] for a in prev_mn], "next": [ix[a] for a in nxt],
            "comeback_penalties": None if cb is None else [ix[a] for a in cb], "prior_penalties": [ix[a] for a in prior], "header_penalties": [ix[a] for a in hp],
            "miners": [ix[a] for a in miners], "hash15": {str(n): h for n, h in h15.items() if n >= N - 900},
            "signs": [[n, ix[a], m] for n, a, m in signs]}

if __name__ == "__main__":
    out, *picks = sys.argv[1:]
    res = []
    for p in picks:
        res.append(build(int(p)))
        print("built", p, flush=True)
    json.dump(res, open(out, "w"), separators=(",", ":"))
