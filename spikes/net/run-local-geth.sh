#!/usr/bin/env bash
# Start a geth-XDC node on 127.0.0.1 only, for the net-spike handshake test.
#   GETH_BIN  path to a geth-XDC binary built from the oracle pin (default: ./geth-xdc)
#   DATADIR   scratch data directory (default: a new mktemp dir)
#   P2P_PORT  listen port (default 30399)
# No discovery, no bootnodes, loopback-only peers, no mining key. Writes <DATADIR>/enode.txt
# once the node is listening, then stays in the foreground until killed.
set -euo pipefail
GETH_BIN=${GETH_BIN:-./geth-xdc}
DATADIR=${DATADIR:-$(mktemp -d)}
P2P_PORT=${P2P_PORT:-30399}
mkdir -p "$DATADIR"
rm -f "$DATADIR/enode.txt"
# --xdcdevnet = chain/network id 5551 with the embedded devnet genesis (core/genesis_xdc.go:84).
"$GETH_BIN" --datadir "$DATADIR" --xdcdevnet --nodiscover --bootnodes "" --nat extip:127.0.0.1 \
  --netrestrict 127.0.0.0/8 --port "$P2P_PORT" --maxpeers 4 --ipcpath "$DATADIR/geth.ipc" \
  --authrpc.addr 127.0.0.1 --authrpc.port "$((P2P_PORT + 1000))" --verbosity 5 \
  > "$DATADIR/geth.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT
for _ in $(seq 1 60); do
  if enode=$(grep -o 'enode://[0-9a-f]*@[^ ]*' "$DATADIR/geth.log" | head -1) && [ -n "$enode" ]; then
    echo "${enode%%@*}@127.0.0.1:$P2P_PORT" | tee "$DATADIR/enode.txt"
    wait "$pid"
    exit 0
  fi
  sleep 1
done
echo "geth did not report an enode; see $DATADIR/geth.log" >&2
exit 1
