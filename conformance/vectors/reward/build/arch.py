"""Read-only archive access for XB-10: ssh xdc02 + curl to 127.0.0.1, <=4 requests/s, cached on disk."""
import json, os, sqlite3, subprocess, time, hashlib
HERE = os.path.dirname(os.path.abspath(__file__))
db = sqlite3.connect(os.path.join(HERE, "cache.db"), timeout=120)
db.execute("create table if not exists c (k text primary key, v text)")
db.execute("create table if not exists blk (port int, n int, v text, primary key(port,n))")
_last = [0.0]
SIGN = "0xe341eaa4"
BS = "0x0000000000000000000000000000000000000089"

def _post(port, body):
    dt = time.time() - _last[0]
    if dt < 0.27:
        time.sleep(0.27 - dt)
    for attempt in range(4):
        p = subprocess.run(["ssh", "-o", "ConnectTimeout=15", "xdc02",
                            f"curl -s -m 180 -X POST -H 'Content-Type: application/json' --data @- http://127.0.0.1:{port}"],
                           input=body.encode(), capture_output=True)
        _last[0] = time.time()
        if p.returncode == 0 and p.stdout:
            try:
                return json.loads(p.stdout)
            except Exception:
                pass
        time.sleep(2 * (attempt + 1))
    raise RuntimeError("archive call failed")

def call(port, method, params):
    k = hashlib.sha256(json.dumps([port, method, params]).encode()).hexdigest()
    r = db.execute("select v from c where k=?", (k,)).fetchone()
    if r:
        return json.loads(r[0])
    out = _post(port, json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
    if "error" in out:
        raise RuntimeError(f"{method}{params}: {out['error']}")
    db.execute("insert or replace into c values(?,?)", (k, json.dumps(out["result"])))
    db.commit()
    return out["result"]

def batch(port, calls):
    """calls: list of (method, params); returns results in order (cached ones skipped)."""
    keys = [hashlib.sha256(json.dumps([port, m, p]).encode()).hexdigest() for m, p in calls]
    res = [None] * len(calls)
    todo = []
    for i, k in enumerate(keys):
        r = db.execute("select v from c where k=?", (k,)).fetchone()
        if r:
            res[i] = json.loads(r[0])
        else:
            todo.append(i)
    for s in range(0, len(todo), 20):
        chunk = todo[s:s + 20]
        body = json.dumps([{"jsonrpc": "2.0", "id": j, "method": calls[i][0], "params": calls[i][1]} for j, i in enumerate(chunk)])
        out = _post(port, body)
        for o in out:
            i = chunk[o["id"]]
            if "error" in o:
                raise RuntimeError(f"{calls[i]}: {o['error']}")
            res[i] = o["result"]
            db.execute("insert or replace into c values(?,?)", (keys[i], json.dumps(o["result"])))
        db.commit()
    return res

def light(b):
    """The part of a full block that the vectors need."""
    signs = []
    for t in b["transactions"]:
        if (t.get("to") or "").lower() == BS and len(t["input"]) == 2 + 136 and t["input"].startswith(SIGN):
            signs.append({"from": t["from"].lower(), "signed": "0x" + t["input"][-64:], "hash": t["hash"]})
    sealer = None
    if b["miner"] == "0x0000000000000000000000000000000000000000" and int(b["number"], 16) < 56828700 and int(b["number"], 16) > 0:
        from v1seal import seal_signer
        try:
            sealer = seal_signer(b)
        except Exception:
            sealer = None
    return {"sealer": sealer, "number": int(b["number"], 16), "hash": b["hash"], "parent": b["parentHash"], "miner": b["miner"].lower(),
            "root": b["stateRoot"], "penalties": b.get("penalties", "0x"), "validators": b.get("validators", "0x"),
            "extra": b["extraData"], "ntx": len(b["transactions"]), "signs": signs}

def _big_block(port, n):
    """A block whose full body does not fit the RPC response limit: the sign transactions are found by the
    log of address 0x89 that each one emits (state_processor.go:899, ApplySigningTransaction) and read one by one."""
    b = call(port, "eth_getBlockByNumber", [hex(n), False])
    logs = call(port, "eth_getLogs", [{"fromBlock": hex(n), "toBlock": hex(n), "address": BS}])
    txs = []
    for h in dict.fromkeys(l["transactionHash"] for l in logs):
        t = call(port, "eth_getTransactionByHash", [h])
        txs.append(t)
    b = dict(b)
    b["transactions"] = txs
    return b

def blocks_via_logs(port, lo, hi):
    """Same result as blocks() for a chain whose blocks hold thousands of transactions (net2040): the sign
    transactions are found by the log of address 0x89 that each emits, then read by hash."""
    hd = hdrs(port, lo, hi)
    logs = []
    for a in range(lo, hi + 1, 1000):
        logs += call(port, "eth_getLogs", [{"fromBlock": hex(a), "toBlock": hex(min(a + 999, hi)), "address": BS}])
    hashes = list(dict.fromkeys(l["transactionHash"] for l in logs))
    txs = {}
    for i in range(0, len(hashes), 40):
        for h, t in zip(hashes[i:i + 40], batch(port, [("eth_getTransactionByHash", [x]) for x in hashes[i:i + 40]])):
            txs[h] = t
    by = {}
    for l in sorted(logs, key=lambda l: (int(l["blockNumber"], 16), int(l["transactionIndex"], 16))):
        t = txs[l["transactionHash"]]
        if t["input"].startswith(SIGN) and len(t["input"]) == 2 + 136 and (t.get("to") or "").lower() == BS:
            by.setdefault(int(l["blockNumber"], 16), []).append({"from": t["from"].lower(), "signed": "0x" + t["input"][-64:], "hash": t["hash"]})
    return [{"number": h["number"], "hash": h["hash"], "miner": h["miner"], "signs": by.get(h["number"], [])} for h in hd]

def blocks(port, lo, hi):
    """light blocks lo..hi inclusive, cached."""
    if os.environ.get("XB10_NET") == "net2040":
        return blocks_via_logs(port, lo, hi)
    have = {r[0] for r in db.execute("select n from blk where port=? and n between ? and ?", (port, lo, hi))}
    need = [n for n in range(lo, hi + 1) if n not in have]
    for s in range(0, len(need), 20):
        chunk = need[s:s + 20]
        body = json.dumps([{"jsonrpc": "2.0", "id": n, "method": "eth_getBlockByNumber", "params": [hex(n), True]} for n in chunk])
        out = _post(port, body)
        for o in out:
            if o.get("result") is None and "too large" in str(o.get("error")):
                o = _post(port, json.dumps({"jsonrpc": "2.0", "id": o["id"], "method": "eth_getBlockByNumber", "params": [hex(o["id"]), True]}))
            if o.get("result") is None and "too large" in str(o.get("error")):
                o = {"id": o["id"], "result": _big_block(port, o["id"])}
            if o.get("result") is None:
                raise RuntimeError(f"block {o['id']}: {o}")
            db.execute("insert or replace into blk values(?,?,?)", (port, o["id"], json.dumps(light(o["result"]))))
        db.commit()
    return [json.loads(r[0]) for r in db.execute("select v from blk where port=? and n between ? and ? order by n", (port, lo, hi))]

db.execute("create table if not exists hdr (port int, n int, v text, primary key(port,n))")

def hdrs(port, lo, hi):
    """headers lo..hi without bodies: (number, hash, miner)."""
    have = {r[0] for r in db.execute("select n from hdr where port=? and n between ? and ?", (port, lo, hi))}
    need = [n for n in range(lo, hi + 1) if n not in have]
    for s in range(0, len(need), 50):
        chunk = need[s:s + 50]
        body = json.dumps([{"jsonrpc": "2.0", "id": n, "method": "eth_getBlockByNumber", "params": [hex(n), False]} for n in chunk])
        out = _post(port, body)
        for o in out:
            if o.get("result") is None:
                raise RuntimeError(f"header {o['id']}: {o}")
            r = o["result"]
            sealer = None
            if r["miner"] == "0x0000000000000000000000000000000000000000" and 0 < int(r["number"], 16) < 56828700:
                from v1seal import seal_signer
                sealer = seal_signer(r)
            db.execute("insert or replace into hdr values(?,?,?)", (port, o["id"], json.dumps({"number": int(r["number"], 16), "hash": r["hash"], "miner": r["miner"].lower(), "sealer": sealer})))
        db.commit()
    return [json.loads(r[0]) for r in db.execute("select v from hdr where port=? and n between ? and ? order by n", (port, lo, hi))]
