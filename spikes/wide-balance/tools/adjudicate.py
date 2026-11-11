"""Legacy adjudication: replay historical Apothem transactions that touch a wide account in the
geth-XDC oracle and compare with the canonical chain.

For each case the pre-state comes from the archive (prestateTracer, with the true balance of any
wide account taken from the proof-verified leaf), the transaction is run by `oracle replay` at its
block number, and the result is compared with (a) the archive's own diff trace (balances, storage)
and (b) the account leaf of the canonical state trie at that block, verified against the header
stateRoot. When the archive cannot trace the transaction (a wide sender), the arithmetic on the
verified leaves is checked instead.

    GETH_SRC=<geth-XDC checkout> python3 adjudicate.py adjudicate_cases.json out.json
"""
import json
import os
import subprocess
import sys
import tempfile

import archive

PORT = archive.APOTHEM
ORACLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "oracle")
WIDE = {"0xa65010026b83368ca05df6e8b467985d6de3eac5", "0x38b20a2594939531373efcd2e6aa54d03fc534be",
        "0xb6c1f9da410d0739efbe934b4c30dfbee2755233", "0x4398241671b3dd484fe3213a4fb7511f30e7d7c0"}
CB = "0x00000000000000000000000000000000000000c0"


def replay(row):
    with tempfile.TemporaryDirectory() as d:
        work = os.path.join(d, "go.work")
        open(work, "w").write("go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n" % (os.path.abspath(ORACLE), os.environ["GETH_SRC"]))
        env = dict(os.environ, GOWORK=work, GOTOOLCHAIN="local", GOFLAGS="")
        out = subprocess.run(["go", "run", ".", "replay"], cwd=ORACLE, env=env, input=json.dumps(row), capture_output=True, text=True)
    assert out.returncode == 0, out.stderr[-2000:]
    return json.loads(out.stdout)


def case(name, blk, txh):
    h, hp = archive.header(PORT, blk), archive.header(PORT, blk - 1)
    tx = archive.rpc(PORT, "eth_getTransactionByHash", [txh])["result"]
    rc = archive.rpc(PORT, "eth_getTransactionReceipt", [txh])["result"]
    base = dict(name=name, block=blk, tx=txh, stateRoot=h["stateRoot"], receiptsRoot=h["receiptsRoot"],
                status=rc["status"], gasUsed=int(rc["gasUsed"], 16), mismatches=[])
    pr = archive.rpc(PORT, "debug_traceTransaction", [txh, {"tracer": "prestateTracer"}])
    if "error" in pr:
        # Tracing wraps the StateDB (hookedStateDB has no GetBigBalance), so a wide sender cannot be traced.
        sender = tx["from"].lower()
        assert sender in WIDE
        a, b = archive.leaf_at(PORT, sender, blk - 1), archive.leaf_at(PORT, sender, blk)
        fee, val = int(rc["gasUsed"], 16) * int(tx["gasPrice"], 16), int(tx["value"], 16)
        txs = archive.rpc(PORT, "eth_getBlockByNumber", [hex(blk), True])["result"]["transactions"]
        tot = sum(int(archive.rpc(PORT, "eth_getTransactionReceipt", [t["hash"]])["result"]["gasUsed"], 16) * int(t["gasPrice"], 16) for t in txs)
        base.update(trace_error=pr["error"]["message"], arith=dict(
            pre_true=hex(a["true"]), post_true=hex(b["true"]), value=hex(val), fee=fee, block_fees=tot,
            equal=a["true"] - val - fee == b["true"], equal_with_block_fees=a["true"] - val - fee + tot == b["true"],
            pre_low=hex(a["low"]), low_lt_value=a["low"] < val))
        return base
    dif = archive.rpc(PORT, "debug_traceTransaction", [txh, {"tracer": "prestateTracer", "tracerConfig": {"diffMode": True}}])["result"]
    accts = {}
    for a, v in pr["result"].items():
        a = a.lower()
        bal = archive.leaf_at(PORT, a, blk - 1)["true"] if a in WIDE else int(v.get("balance", "0x0"), 16)
        accts[a] = dict(balance=hex(bal), nonce=v.get("nonce", 0), code=v.get("code", "") or "", storage=v.get("storage", {}))
    accts.setdefault(CB, dict(balance="0x0", nonce=0, code="", storage={}))
    slots = {a.lower(): list(v["storage"]) for a, v in dif["post"].items() if v.get("storage")}
    row = dict(id=name, matrix_row="replay", rule="replay", block=blk, time=int(h["timestamp"], 16), accounts=accts,
               ops=[{"op": "tx", "from": tx["from"].lower(), "to": (tx["to"] or "").lower(), "value": hex(int(tx["value"], 16)),
                     "gas": int(tx["gas"], 16), "gas_price": hex(int(tx["gasPrice"], 16)), "data": tx["input"], "coinbase": CB}],
               watch=sorted(set(accts) | {a.lower() for a in dif["post"]}), slots=slots,
               expect=dict(state_root="", txs=[], accounts={}))
    o = replay(row)
    base["oracleTx"] = o["txs"][0]
    for a, v in dif["post"].items():
        a = a.lower()
        oa = o["accounts"].get(a)
        if oa is None or not oa["exists"]:
            if int(v.get("balance", "0x0"), 16) or v.get("storage") or v.get("code"):
                base["mismatches"].append((a, "absent in oracle"))
            continue
        if "balance" in v and a != CB and int(oa["low"], 16) != int(v["balance"], 16):
            base["mismatches"].append((a, "low", oa["low"], v["balance"]))
        for k, sv in v.get("storage", {}).items():
            if int(oa["storage"][k], 16) != int(sv, 16):
                base["mismatches"].append((a, "slot", k, oa["storage"][k], sv))
    base["wide"] = {}
    for a in (x for x in accts if x in WIDE):
        post = archive.leaf_at(PORT, a, blk)
        oa = o["accounts"][a]
        base["wide"][a] = dict(pre_true=accts[a]["balance"], post_true_canonical=hex(post["true"]), oracle_true=oa["true"],
                               oracle_low=oa["low"], canonical_low=hex(post["low"]), leaf_rlp_equal=oa["rlp"][2:] == post["rlp"])
        if int(oa["true"], 16) != post["true"]:
            base["mismatches"].append((a, "true", oa["true"], hex(post["true"])))
    return base


if __name__ == "__main__":
    res = [case(*c) for c in json.load(open(sys.argv[1]))]
    json.dump(res, open(sys.argv[2], "w"), indent=1)
    for r in res:
        print(r["name"], r["block"], "mismatches", r["mismatches"])
