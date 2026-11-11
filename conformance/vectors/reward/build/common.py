import sys, json, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import arch
from Crypto.Hash import keccak
P = int(os.environ.get("XB10_PORT", "21105"))
NET = os.environ.get("XB10_NET", "apothem")
SMC = "0x0000000000000000000000000000000000000088"
_REPO = os.environ.get("XONE_ROOT", os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../..")))
_FORKS = (_REPO + "/crates/xone-chainspec/generated/forks.json") if NET != "net2040" else (_REPO + "/conformance/vectors/consensus/net2040.forks.json")
NETCFG = json.load(open(_FORKS))["networks"][NET]
FOUNDATION = NETCFG["xdpos"]["foudationWalletAddr"].lower()

def k256(b):
    return keccak.new(digest_bits=256, data=b).digest()

def pad32(h):
    return bytes.fromhex(h[2:]).rjust(32, b"\0")

def owner_slot(node):
    return int.from_bytes(k256(pad32(node) + (1).to_bytes(32, "big")), "big")

def addrs(hexbytes):
    return ["0x" + hexbytes[2:][i:i + 40] for i in range(0, len(hexbytes) - 2, 40)]

def owners(nodes, block):
    res = arch.batch(P, [("eth_getStorageAt", [SMC, hex(owner_slot(n)), hex(block)]) for n in nodes])
    return ["0x" + r[-40:] for r in res]

def balances(accts, block):
    res = arch.batch(P, [("eth_getBalance", [a, hex(block)]) for a in accts])
    return [int(r, 16) for r in res]

def tx_delta(n):
    """sum over the transactions of the block of post - pre balance, per account (prestateTracer, diffMode)."""
    r = arch.call(P, "debug_traceBlockByNumber", [hex(n), {"tracer": "prestateTracer", "tracerConfig": {"diffMode": True}}])
    d = {}
    for t in r:
        res = t["result"]
        pre, post = res.get("pre", {}), res.get("post", {})
        for a in set(pre) | set(post):
            b0 = int(pre.get(a, {}).get("balance", "0x0"), 16)
            if a in post and "balance" in post[a]:
                b1 = int(post[a]["balance"], 16)
            elif a in post or a not in pre:
                b1 = b0
            else:
                b1 = 0  # deleted
            d[a.lower()] = d.get(a.lower(), 0) + b1 - b0
    return d

def fees(n):
    """gasUsed * effectiveGasPrice over the non-special transactions of block n (fee credit of the owner of the author)."""
    blk = arch.call(P, "eth_getBlockByNumber", [hex(n), True])
    rec = arch.call(P, "eth_getBlockReceipts", [hex(n)])
    total = 0
    for t, r in zip(blk["transactions"], rec):
        to = (t.get("to") or "").lower()
        if to in ("0x0000000000000000000000000000000000000089", "0x0000000000000000000000000000000000000090"):
            continue
        total += int(r["gasUsed"], 16) * int(r.get("effectiveGasPrice", t.get("gasPrice", "0x0")), 16)
    return total, blk["miner"].lower(), blk["stateRoot"]

def header(n):
    return arch.call(P, "eth_getBlockByNumber", [hex(n), False])
