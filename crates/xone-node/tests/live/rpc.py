"""One JSON-RPC call over a unix socket (geth IPC) or HTTP; prints the `result` as compact JSON.
Usage: rpc.py ipc|http TARGET METHOD [PARAMS_JSON]"""
import json
import socket
import sys
import urllib.request

kind, target, method = sys.argv[1:4]
params = json.loads(sys.argv[4]) if len(sys.argv) > 4 else []
req = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
if kind == "ipc":
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(10)
    s.connect(target)
    s.sendall(json.dumps(req).encode())
    buf = b""
    while True:
        buf += s.recv(65536)
        try:
            out = json.loads(buf)
            break
        except ValueError:
            continue
else:
    r = urllib.request.Request(target, json.dumps(req).encode(), {"content-type": "application/json"})
    out = json.load(urllib.request.urlopen(r, timeout=10))
if "error" in out:
    print(json.dumps(out["error"], sort_keys=True))
    sys.exit(2)
print(json.dumps(out["result"], sort_keys=True, separators=(",", ":")))
