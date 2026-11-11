"""Gap-snapshot vectors (XB-11): V2 epoch switches of the Apothem and net2040 archives, with the 0x88 storage words of the
gap block as inputs and the switch header's Validators and Penalties as expected values.

  python3 -I gap.py apothem   ../gap-apothem.json     (needs ssh xdc02; read-only; at most 4 requests per second, cached)
  XB11_NET=net2040 python3 -I gap.py net2040 ../gap-net2040.json
"""
import sys, os, json, random
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../reward/build"))
import arch
from Crypto.Hash import keccak

net, out = sys.argv[1], sys.argv[2]
PORT = {"apothem": 21105, "net2040": 10071}[net]
SMC = "0x0000000000000000000000000000000000000088"
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../.."))
SWITCH, EPOCH, GAP = {"apothem": (56828700, 900, 450), "net2040": (10800, 900, 450)}[net]
HEAD = {"apothem": 85200000, "net2040": 730000}[net]
COUNT = {"apothem": 100, "net2040": 20}[net]


def k256(b):
    return keccak.new(digest_bits=256, data=b).digest()


def pad32(x):
    return x.rjust(32, b"\0")


def hx(b):
    return "0x" + b.hex()


def call(m, p):
    return arch.call(PORT, m, p)


def numbers_of(path):
    """Block numbers the other vector sets already use, so that those switches are covered here too."""
    out = set()
    for line in open(path):
        v = json.loads(line)
        if v["network"] == net and v["class"] in ("v2_epoch",):
            out.add(v["number"])
    return out


def switch_at_or_after(h):
    r = call("XDPoS_getEpochNumbersBetween", [hex(h), hex(h + 2000)])
    return r[0] if r else None


def addrs(b):
    b = b[2:]
    return ["0x" + b[i:i + 40] for i in range(0, len(b), 40)]


def proof_words(slots):
    """The values of storage slots of 0x88 at the block of `block`, one request (eth_getProof)."""
    raise NotImplementedError


def words_at(block, slots):
    res = []
    for i in range(0, len(slots), 120):
        chunk = slots[i:i + 120]
        r = call("eth_getProof", [SMC, [hx(s.to_bytes(32, "big")) for s in chunk], hex(block)])
        for sp in r["storageProof"]:
            res.append(int(sp["value"], 16))
    return res


def switch_record(S):
    hdr = call("eth_getBlockByNumber", [hex(S), False])
    mn = call("XDPoS_getMasternodesByNumber", [hex(S)])
    assert mn["Number"] == S, (S, mn["Number"])
    snap = S - 1 if S % EPOCH == EPOCH - GAP else S
    G = snap - snap % EPOCH
    G = G - GAP if G > GAP else 0
    gh = call("eth_getBlockByNumber", [hex(G), False])
    n = words_at(G, [8])[0]
    arr_base = int.from_bytes(k256(pad32((8).to_bytes(32, "big"))), "big")
    elems = words_at(G, [arr_base + i for i in range(n)])
    cands = sorted({"0x" + w.to_bytes(32, "big")[12:].hex() for w in elems if w})
    caps_slots = []
    for c in cands:
        loc = int.from_bytes(k256(pad32(bytes.fromhex(c[2:])) + (1).to_bytes(32, "big")), "big") + 1
        caps_slots.append(loc)
    caps = words_at(G, caps_slots)
    words = [[hx((8).to_bytes(32, "big")), hx(n.to_bytes(32, "big"))]]
    words += [[hx((arr_base + i).to_bytes(32, "big")), hx(w.to_bytes(32, "big"))] for i, w in enumerate(elems) if w]
    words += [[hx(s.to_bytes(32, "big")), hx(w.to_bytes(32, "big"))] for s, w in zip(caps_slots, caps)]
    validators, penalties = addrs(hdr.get("validators", "0x")), addrs(hdr.get("penalties", "0x"))
    assert [a.lower() for a in mn["Masternodes"]] == validators, S
    assert sorted(a.lower() for a in (mn.get("Penalty") or [])) == sorted(penalties), S
    return {"id": f"{net}-{S}", "network": net, "number": S, "hash": hdr["hash"], "round": mn["Round"], "gap": G, "gap_hash": gh["hash"],
            "words": words, "validators": validators, "penalties": penalties}


def bootstrap_record():
    S = SWITCH + 1
    hdr = call("eth_getBlockByNumber", [hex(S), False])
    sw = call("eth_getBlockByNumber", [hex(SWITCH), False])
    mn = call("XDPoS_getMasternodesByNumber", [hex(S)])
    validators, penalties = addrs(hdr.get("validators", "0x")), addrs(hdr.get("penalties", "0x"))
    assert [a.lower() for a in mn["Masternodes"]] == validators
    return {"id": f"{net}-{S}-bootstrap", "network": net, "number": S, "hash": hdr["hash"], "round": mn["Round"] if mn["Number"] == S else 1,
            "switch_extra": sw["extraData"], "validators": validators, "penalties": penalties}


want = set()
for f in ("penalty/apothem-v2.json", "penalty/net2040-v2.json"):
    for r in json.load(open(os.path.join(ROOT, "conformance/vectors", f))):
        if r["network"] == net:
            want.add(r["number"])
want |= numbers_of(os.path.join(ROOT, "conformance/vectors/consensus/valid", net + ".jsonl"))
rnd = random.Random(11)
while len(want) < COUNT:
    s = switch_at_or_after(rnd.randrange(SWITCH + 1000, HEAD))
    if s and s > SWITCH + 1:
        want.add(s)
want = sorted(want)[:COUNT] if len(want) > COUNT else sorted(want)
res = {"switches": [], "bootstrap": bootstrap_record()}
for i, S in enumerate(want):
    res["switches"].append(switch_record(S))
    print(i + 1, len(want), S, len(res["switches"][-1]["validators"]), len(res["switches"][-1]["penalties"]), flush=True)
json.dump(res, open(out, "w"), separators=(",", ":"))
