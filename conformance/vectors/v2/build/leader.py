"""Leader vectors (XB-11): 50 consecutive V2 blocks around an epoch switch, with the masternode list of each epoch
(XDPoS_getMasternodesByNumber) and the round (header extraData) and author (coinbase) of every block.

  python3 -I leader.py apothem ../leader-apothem.json
"""
import sys, os, json
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../reward/build"))
import arch

net, out = sys.argv[1], sys.argv[2]
PORT = {"apothem": 21105, "net2040": 10071}[net]
here = os.path.dirname(os.path.abspath(__file__))
gap = json.load(open(os.path.join(here, f"../gap-{net}.json")))["switches"]
S = gap[len(gap) // 2]["number"]


def rlp_round(extra):
    """extraData = 0x02 || rlp([round, qc]): the first item of the list is the round."""
    b = bytes.fromhex(extra[2:])
    assert b[0] == 2
    p = b[1:]
    i = 1 if p[0] < 0xf8 else 1 + (p[0] - 0xf7)
    f = p[i]
    if f < 0x80:
        return f
    n = f - 0x80
    return int.from_bytes(p[i + 1:i + 1 + n], "big")


blocks = []
for n in range(S - 25, S + 25):
    h = arch.call(PORT, "eth_getBlockByNumber", [hex(n), False])
    blocks.append({"number": n, "round": rlp_round(h["extraData"]), "miner": h["miner"]})
lists = []
for n in (S - 25, S):
    m = arch.call(PORT, "XDPoS_getMasternodesByNumber", [hex(n)])
    lists.append({"from_number": m["Number"], "masternodes": m["Masternodes"]})
assert lists[1]["from_number"] == S and lists[0]["from_number"] < S
json.dump({"network": net, "switch": S, "epoch": 900, "blocks": blocks, "lists": lists}, open(out, "w"), indent=1)
print(net, S, len(blocks))
