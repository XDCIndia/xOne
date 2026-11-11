"""Find every wide Apothem account: follow flows of at least 2^199 wei out of the nine genesis
accounts funded with 2^256-1, then read the verified true balance of each account found.

Heavy accounts are the only candidates (a balance above 2^256-1 needs an account that already
holds close to 2^256). A flow below 2^199 cannot create a new heavy account, so the walk only
follows top-level transfers of at least that size. Limit: a transfer that leaves and returns
between two samples, or an internal (contract) transfer of that size, would be missed; none of
the drops found was unexplained.

    python3 explore.py 0x<seed> ...        # prints heavy accounts and their big flows
"""
import functools
import sys

import archive

PORT, HEAD = archive.APOTHEM, 0x5147832
BIG = 2**199
GENESIS_MAX = [
    "0x03c0d9bc556be68870b96976e81d32ebb49d335d", "0x065551f0dcac6f00cae11192d462db709be3758c",
    "0x38b20a2594939531373efcd2e6aa54d03fc534be", "0x42e694adfd403152cd9cad82a62fb6cd403f150a",
    "0x4398241671b3dd484fe3213a4fb7511f30e7d7c0", "0x4e37f91e3bc69725326af96facf85c14128b07ed",
    "0x7aa125338be075260e77c6a66a56c90a5dec4c58", "0x9a3787688fd210ec8f8d0224c6c50b8178d75bc0",
    "0xa65010026b83368ca05df6e8b467985d6de3eac5",
]


@functools.lru_cache(maxsize=None)
def bal(a, b):
    try:
        return archive.leaf_at(PORT, a, b, check=False)["true"]
    except AssertionError:
        return 0  # account absent at that block


def first_change(a, lo, hi):
    base = bal(a, lo)
    if abs(bal(a, hi) - base) < BIG:
        return None
    while hi - lo > 1:
        m = (lo + hi) // 2
        lo, hi = (lo, m) if abs(bal(a, m) - base) >= BIG else (m, hi)
    return hi


def changes(a):
    pts = list(range(0, HEAD, 2_000_000)) + [HEAD]
    out = []
    for lo, hi in zip(pts, pts[1:]):
        cur = lo
        while (c := first_change(a, cur, hi)) is not None:
            out.append((c, bal(a, c - 1), bal(a, c)))
            cur = c
    return out


def explore(seed):
    seen, queue = {}, list(seed)
    while queue:
        a = queue.pop(0)
        if a in seen:
            continue
        seen[a] = changes(a)
        print("ACCT", a, "big changes:", [(c, "-" if n < p else "+", hex(abs(n - p))) for c, p, n in seen[a]], flush=True)
        for c, _, _ in seen[a]:
            for t in archive.rpc(PORT, "eth_getBlockByNumber", [hex(c), True])["result"]["transactions"]:
                if int(t["value"], 16) >= BIG and a in (t["from"].lower(), (t["to"] or "").lower()):
                    print("   block", c, "tx", t["hash"], t["from"], "->", t["to"], hex(int(t["value"], 16)), flush=True)
                    queue += [x for x in (t["from"].lower(), (t["to"] or "").lower()) if x and x not in seen]
    return seen


if __name__ == "__main__":
    explore(sys.argv[1:] or GENESIS_MAX)
