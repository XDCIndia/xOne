"""Synthetic reward windows (voters, owner = foundation, shared owners, all three tiers): inputs are made up, the expected maps come from the pinned geth functions."""
import json, os, random
rnd = random.Random(20261111)
FOUND = "0x746249c61f5832c5eed53172776b460491bdcd5c"
def addr(i): return "0x%040x" % (0x5000 + i)
def h(): return "0x%064x" % rnd.getrandbits(256)

def make(number, kind, rnum, nm, np_, no, ns, owner_found_idx=None, tag=""):
    # nm masternodes, np_ protectors, no observers, ns standby (the pool is protectors + observers + standby, in this order)
    mn = [addr(i) for i in range(nm)]
    pool = [addr(100 + i) for i in range(np_ + no + ns)]
    prot, obs = (pool[:3], pool[3:5]) if kind == "tiered" else ([], [])
    nodes = mn + prot + obs
    owners = [addr(200 + i) for i in range(len(nodes))]
    if owner_found_idx is not None:
        owners[owner_found_idx] = FOUND
    owners[1] = owners[2]  # two nodes, one owner
    voters = {}
    for i, n in enumerate(nodes):
        vs = [[addr(300 + (i + j) % 9), str(10**18 * (1 + (i * j) % 7))] for j in range(3)]
        vs.append(vs[0][:])           # a voter listed twice
        vs.append([owners[i], str(5 * 10**18)])   # the owner votes for its own node
        vs.append([FOUND, str(10**18)])           # the foundation votes
        vs.append([addr(400 + i), "0"])           # a voter with no stake
        voters[n] = vs
    end = number - 900 - 1
    start = end - 899
    if kind == "v1":
        start, end = number - 1799, number - 900
    wh = {str(k): h() for k in range(start, end + 1) if k % 15 == 0}
    p = {a: rnd.choice([1.0, 1.0, 0.9, 0.5, 0.2, 0.0]) for a in nodes}
    signed = {k: [a for a in nodes if rnd.random() < p[a]] for k in wh}
    return {"number": number, "kind": kind, "round": rnum, "start": start, "end": end, "parent_root": h(), "parent_hash": h(),
            "pool": pool if kind == "tiered" else [], "nodes": nodes, "owners": owners, "masternodes": mn, "protectors": prot, "observers": obs,
            "window_hash": wh, "signed": signed, "voters": voters}

recs = [
    make(83_700_000, "tiered", 27_380_000, 9, 4, 3, 2, owner_found_idx=0),   # fixed tiered, owner is the foundation
    make(85_000_000, "tiered", 27_380_000, 9, 4, 3, 2, owner_found_idx=3),   # proportional, a protector's owner is the foundation
    make(85_000_900, "tiered", 27_380_000, 6, 5, 5, 0),                      # proportional, short pools
    make(80_000_100, "v2", 0, 10, 0, 0, 0, owner_found_idx=0),               # V2 flat, owner is the foundation: its 90% is not paid
    make(10_000_800, "v1", 0, 12, 0, 0, 0, owner_found_idx=4),
]
os.makedirs("rawsyn", exist_ok=True)
for r in recs:
    json.dump(r, open(f"rawsyn/{r['number']}.json", "w"))
print(len(recs))
