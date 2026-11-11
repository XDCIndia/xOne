"""Record the geth-XDC archive's answers to every in-scope RPC method at 50 blocks per network (XB-22).

Read-only: ssh xdc02 + curl to 127.0.0.1, 4 HTTP requests per second, at most 5 calls per request.
Never edit a vector by hand: run this again (python3 build.py apothem|net2040 [block,block,...]: the second form records only those blocks and merges them in; Apothem 16,17,76,122,917,1756,1772,1787 are the blocks of the replayed window that carry transactions).
Output: ../<network>/<method>.jsonl, one line per call: {network, block, class, request, response}.
The response is the archive's answer verbatim (errors included). Calls that depend on the live head
or on the node (class "live") are recorded once, at the head, and are not state-derived.
"""
import json, os, subprocess, sys, time

NETS = {
    "apothem": dict(port=21105, blocks=[
        0, 1, 2, 3, 4, 5, 899, 900, 901, 1799, 1800, 1801, 2700, 9000, 450000,
        56827800, 56828699, 56828700, 56828701, 56829600,  # V1 to V2 switch (56828700) and the next epoch
        61289999, 61290000, 61290001,  # berlin / london / istanbul row
        71549999, 71550000, 71551800, 71551801,  # eip1559 / cancun
        83599999, 83600000, 83600001, 84332999, 84333000, 84333001,  # tipFixedReward / dynamicGasLimit / prague
        84732999, 84733000, 84733001, 84900000, 85000000, 85100700,  # tipProportionalReward, V2 epoch switches
        85139999, 85140000, 85140001, 85141800, 85141900, 85142000, 85142100, 85142200, 85142300, 85142400, 85142500]),
    "net2040": dict(port=10071, blocks=[
        0, 1, 2, 3, 4, 5, 899, 900, 901, 1800, 3600, 5400, 7200, 9000, 9001, 10799, 10800, 10801,
        10900, 11700, 12600, 12601, 14400, 16200, 16201, 18000, 19800, 21600, 21601, 30600,
        50400, 90000, 150300, 200700, 301500, 400500, 500400, 600300, 700200, 740700, 745200,
        746100, 746999, 747000, 747001, 747002, 747003, 747004, 747005, 747006]),
}
SMC = "0x0000000000000000000000000000000000000088"
FOUND = {"apothem": "0x746249c61f5832c5eed53172776b460491bdcd5c", "net2040": "0x1c1d49ed32312a524bf996f4adb08b4ccf573112"}
GET_CANDIDATES = "0x06a49fce"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
MAX_BYTES = 1 << 20
_last = [0.0]


def post(port, body):
    dt = time.time() - _last[0]
    if dt < 0.26:
        time.sleep(0.26 - dt)
    for attempt in range(4):
        p = subprocess.run(["ssh", "-o", "ConnectTimeout=15", "xdc02", f"curl -s -m 120 -X POST -H 'Content-Type: application/json' --data @- http://127.0.0.1:{port}"],
                           input=body.encode(), capture_output=True)
        _last[0] = time.time()
        if p.returncode == 0 and p.stdout:
            try:
                return json.loads(p.stdout)
            except Exception:
                pass
        time.sleep(2 * (attempt + 1))
    raise RuntimeError("archive call failed")


def run(port, calls):
    """calls: [(method, params)] -> [response objects], 5 per HTTP request."""
    out = []
    for s in range(0, len(calls), 5):
        chunk = calls[s:s + 5]
        body = json.dumps([{"jsonrpc": "2.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(chunk)])
        r = post(port, body)
        by = {o["id"]: o for o in r} if isinstance(r, list) else {}
        for i, (m, p) in enumerate(chunk):
            if i in by:
                out.append(by[i])
            else:  # the whole batch failed (response too large): ask one by one
                single = post(port, json.dumps({"jsonrpc": "2.0", "id": 0, "method": m, "params": p}))
                out.append(single if isinstance(single, dict) else {"jsonrpc": "2.0", "id": 0, "error": {"code": -1, "message": "no answer"}})
    return out


def main(net, only=None):
    """`only`: record just these blocks (no live rows) and merge them into the existing files."""
    cfg = dict(NETS[net])
    if only:
        cfg["blocks"] = only
    port = cfg["port"]
    rows = {}

    def rec(block, cls, calls):
        for (m, p), resp in zip(calls, run(port, calls)):
            resp = dict(resp)
            resp["id"] = 1
            if len(json.dumps(resp)) > MAX_BYTES:
                resp = {"jsonrpc": "2.0", "id": 1, "omitted": "response over 1 MiB", "size": len(json.dumps(resp))}
            rows.setdefault(m, []).append({"network": net, "block": block, "class": cls,
                                           "request": {"jsonrpc": "2.0", "id": 1, "method": m, "params": p}, "response": resp})

    head = int(run(port, [("eth_blockNumber", [])])[0]["result"], 16)
    live = [("eth_blockNumber", []), ("eth_chainId", []), ("eth_gasPrice", []), ("eth_syncing", []), ("eth_mining", []), ("eth_coinbase", []),
            ("net_version", []), ("net_listening", []), ("net_peerCount", []), ("web3_clientVersion", []), ("web3_sha3", ["0x68656c6c6f20776f726c64"]),
            ("txpool_status", []), ("txpool_inspect", []), ("admin_nodeInfo", []), ("admin_peers", []),
            ("XDPoS_getLatestPoolStatus", []), ("XDPoS_networkInformation", []), ("eth_getBlockByNumber", ["latest", False]),
            ("eth_getBlockByNumber", ["0x7fffffff", False]), ("eth_feeHistory", ["0x4", "latest", [50]]), ("net_version", []),
            ("eth_getBlockSignersByNumber", ["0x7fffffff"]), ("XDPoS_getSnapshot", ["0x7fffffff"]), ("nosuch_method", []), ("xdpos_getSigners", ["latest"]),
            ("XDPoS_getSigners", ["latest"]), ("eth_getBalance", [SMC, "latest"])]
    if not only:
        rec(None, "live", live)
    for b in cfg["blocks"]:
        if b > head:
            continue
        hx = hex(b)
        blk = run(port, [("eth_getBlockByNumber", [hx, False])])[0].get("result")
        if not blk:
            continue
        h = blk["hash"]
        miner = blk.get("miner") or SMC
        epoch = b // 900
        rec(b, "block", [
            ("eth_getBlockByNumber", [hx, False]), ("eth_getBlockByNumber", [hx, True]), ("eth_getBlockByHash", [h, False]), ("eth_getBlockByHash", [h, True]),
            ("eth_getBlockTransactionCountByNumber", [hx]), ("eth_getBlockTransactionCountByHash", [h]), ("eth_getBlockReceipts", [hx]),
            ("eth_getUncleCountByBlockNumber", [hx]), ("debug_getRawHeader", [hx]), ("debug_getRawBlock", [hx]),
            ("eth_getLogs", [{"fromBlock": hx, "toBlock": hx}]), ("eth_getBlockSignersByNumber", [hx]), ("eth_getBlockSignersByHash", [h]),
            ("eth_getBlockFinalityByNumber", [hx]), ("eth_getBlockFinalityByHash", [h]), ("eth_getMasternodes", [hx]), ("eth_getCandidateStatus", [FOUND[net], hx]),
        ])
        rec(b, "state", [
            ("eth_getBalance", [SMC, hx]), ("eth_getBalance", ["xdc" + FOUND[net][2:], hx]), ("eth_getBalance", [miner, hx]),
            ("eth_getCode", [SMC, hx]), ("eth_getStorageAt", [SMC, "0x3", hx]), ("eth_getTransactionCount", [miner, hx]),
            ("eth_call", [{"to": SMC, "data": GET_CANDIDATES}, hx]), ("eth_call", [{"to": "xdc" + SMC[2:], "data": GET_CANDIDATES}, hx]),
            ("eth_estimateGas", [{"to": SMC, "data": GET_CANDIDATES}, hx]),
        ])
        rec(b, "xdpos", [
            ("XDPoS_getSnapshot", [hx]), ("XDPoS_getSnapshotAtHash", [h]), ("XDPoS_getSigners", [hx]), ("XDPoS_getSignersAtHash", [h]),
            ("XDPoS_getMasternodes", [hx]), ("XDPoS_getCandidates", [hx]), ("XDPoS_getMasternodesByNumber", [hx]),
            ("XDPoS_getMissedRoundsInEpochByBlockNum", [hx]), ("XDPoS_getV2BlockByNumber", [hx]), ("XDPoS_getBlockInfoByEpochNum", [epoch]),
            ("XDPoS_getEpochNumbersBetween", [hx, hex(min(b + 2700, head))]), ("XDPoS_getEpochNumbersBetween", [hx, hx]),  # begin == end (CertiK XDC-30)
            ("XDPoS_getRewardByAccount", [FOUND[net], hx, hx]), ("xdpos_getSigners", [hx]),
        ])
        if blk["transactions"]:
            t = blk["transactions"][0]
            full = run(port, [("eth_getTransactionByHash", [t])])[0].get("result") or {}
            rec(b, "tx", [
                ("eth_getTransactionByHash", [t]), ("eth_getTransactionByBlockNumberAndIndex", [hx, "0x0"]), ("eth_getTransactionByBlockHashAndIndex", [h, "0x0"]),
                ("eth_getTransactionReceipt", [t]),
                ("debug_traceTransaction", [t, {"tracer": "callTracer"}]), ("debug_traceTransaction", [t, {"tracer": "prestateTracer"}]),
                ("debug_traceBlockByNumber", [hx, {"tracer": "callTracer"}]), ("debug_traceBlockByHash", [h, {"tracer": "prestateTracer"}]),
                ("eth_getTransactionCount", [full.get("from", miner), hx]),
            ])
        else:
            rec(b, "tx", [("eth_getTransactionByBlockNumberAndIndex", [hx, "0x0"]), ("debug_traceBlockByNumber", [hx, {"tracer": "callTracer"}])])
    for m, rs in rows.items():
        d = os.path.join(OUT, net)
        os.makedirs(d, exist_ok=True)
        path = os.path.join(d, m + ".jsonl")
        if only and os.path.exists(path):
            keep = [json.loads(l) for l in open(path) if json.loads(l)["block"] not in only]
            rs = keep + rs
        with open(path, "w") as f:
            for r in rs:
                f.write(json.dumps(r, sort_keys=True, separators=(",", ":")) + "\n")
    print(net, "head", head, "methods", len(rows), "calls", sum(map(len, rows.values())))


if __name__ == "__main__":
    main(sys.argv[1], [int(x) for x in sys.argv[2].split(",")] if len(sys.argv) > 2 else None)
