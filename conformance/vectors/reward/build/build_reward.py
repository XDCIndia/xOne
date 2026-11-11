"""One reward vector: archive inputs (window, sets, owners, parent root) and archive truth (balance deltas)."""
import sys, os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sys, json
from common import *

def epoch_switches(n, back=3500):
    lo = max(n - back, int(NETCFG["xdpos"]["v2"]["switchBlock"]) + 1)
    return sorted(set(arch.call(P, "XDPoS_getEpochNumbersBetween", [hex(lo), hex(n)])))

def v1_masternodes(h):
    ex = h["extraData"][2:]
    body = ex[64:-130]
    return ["0x" + body[i:i + 40] for i in range(0, len(body), 40)]

FORKS = NETCFG["xdpos"]["v2"]["allConfigs"]

def cfg_at(rnd):
    k = max(int(x) for x in FORKS if int(x) <= rnd)
    return FORKS[str(k)]

def build(n, kind):
    """kind: v1 (checkpoint every 900), v2 (flat), tiered"""
    pool = []
    if kind == "v1":
        e2, e1 = n - 1800, n - 900
        start, end = e2 + 1, e1
        mn = v1_masternodes(header(e2))
        rnd = 0
        prot = obs = []
    elif kind == "v2edge":
        # the V2 walk crosses the V1 checkpoint of the switch block, a switch by block number
        e1, e2 = {56829393: (56828700, 56827800), 56830293: (56829393, 56828700)}[n]
        start, end = e2 + 1, e1 - 1
        mn = v1_masternodes(header(e2))
        rnd = arch.call(P, "XDPoS_getMasternodesByNumber", [hex(n)])["Round"]
        prot = obs = []
    else:
        sw = epoch_switches(n)
        assert sw[-1] == n, (n, sw[-3:])
        e1, e2 = sw[-2], sw[-3]
        assert e2 != int(NETCFG["xdpos"]["v2"]["switchBlock"]) + 1, "second switch is the V2 switch block plus one"
        start, end = e2 + 1, e1 - 1
        mn = arch.call(P, "XDPoS_getMasternodesByNumber", [hex(e2)])["Masternodes"]
        mn = [a.lower() for a in mn]
        assert mn == addrs(header(e2)["validators"]), "masternodes differ from the header"
        rnd = arch.call(P, "XDPoS_getMasternodesByNumber", [hex(n)])["Round"]
        prot, obs = [], []
        if kind == "tiered":
            st = arch.call(P, "XDPoS_getMasternodesByNumber", [hex(e2)])
            pool = [a.lower() for a in (st.get("Protectornodes") or []) + (st.get("Observernodes") or []) + (st.get("Standbynodes") or [])]
            c = cfg_at(rnd)
            pe = min(c["maxProtectorNodes"], len(pool))
            oe = min(pe + c["maxObserverNodes"], len(pool))
            prot, obs = pool[:pe], pool[pe:oe]
    blocks = arch.blocks(P, start, n - 1)
    by = {b["number"]: b for b in blocks}
    window_hash = {k: by[k]["hash"] for k in range(start, end + 1) if k % 15 == 0}
    members = set(mn) | set(prot) | set(obs)
    signed = {k: [] for k in window_hash}
    inv = {h: k for k, h in window_hash.items()}
    for b in blocks:
        for s in b["signs"]:
            k = inv.get(s["signed"])
            if k is not None and s["from"] in members and s["from"] not in signed[k]:
                signed[k].append(s["from"])
    nodes = list(dict.fromkeys(mn + prot + obs))
    own = owners(nodes, n - 1)
    acct = sorted(set(own) | {FOUNDATION})
    b0, b1 = balances(acct, n - 1), balances(acct, n)
    td = tx_delta(n)
    fee, miner, root = fees(n)
    par = header(n - 1)
    fee_owner = None
    if miner != "0x0000000000000000000000000000000000000000":
        fee_owner = own[nodes.index(miner)] if miner in nodes else owners([miner], n - 1)[0]
    delta = {a: b1[i] - b0[i] - td.get(a, 0) for i, a in enumerate(acct)}
    return {"number": n, "kind": kind, "round": rnd, "start": start, "end": end, "parent_root": par["stateRoot"],
            "parent_hash": par["hash"], "pool": pool, "nodes": nodes, "owners": own, "masternodes": mn, "protectors": prot, "observers": obs,
            "window_hash": {str(k): v for k, v in window_hash.items()}, "signed": {str(k): v for k, v in signed.items()},
            "fee": fee, "miner": miner, "fee_owner": fee_owner, "delta": delta, "accounts": acct, "e1": e1, "e2": e2}

if __name__ == "__main__":
    n, kind = int(sys.argv[1]), sys.argv[2]
    r = build(n, kind)
    print(json.dumps({k: (v if k not in ("signed", "window_hash", "nodes", "owners", "masternodes", "accounts") else len(v)) for k, v in r.items()}, indent=1))
