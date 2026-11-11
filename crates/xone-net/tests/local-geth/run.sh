#!/usr/bin/env bash
# A geth-XDC node on 127.0.0.1 only, for crates/xone-net/tests/local_geth.rs. It does not run
# discovery, has no bootnodes, accepts loopback peers only (the same flags as
# spikes/net/run-local-geth.sh) and runs a private chain with one signer that we own, so that
# blocks 1..N exist to be requested.
#
#   XONE_GETH_BIN  geth-XDC built from the oracle pin (conformance/vectors/net/oracle/PIN)
#   DATADIR   working directory (default: a new mktemp dir); kept between runs
#   P2P_PORT  listen port (default 30399)
#   MODE      prepare: mine blocks until BLOCKS exist, then exit.  serve (default): no mining.
#   BLOCKS    chain length for prepare (default 20)
#   HOLDER    command that connects a genesis-head peer (needed once: see below)
#
# Why a holder: the XDC miner is gated until it has a peer whose head is the genesis
# (eth/sync_xdc.go:812-838). `prepare` therefore runs $HOLDER (the ignored test
# `hold_genesis_peer`) while mining. `serve` writes enode.txt and genesis.txt.
set -euo pipefail
GETH_BIN=${XONE_GETH_BIN:-./geth-xdc}
DATADIR=${DATADIR:-$(mktemp -d)}
P2P_PORT=${P2P_PORT:-30399}
MODE=${MODE:-serve}
BLOCKS=${BLOCKS:-20}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$DATADIR"
IPC="$DATADIR/geth.ipc"
common=(--datadir "$DATADIR" --networkid 5551 --nodiscover --bootnodes "" --nat extip:127.0.0.1
  --netrestrict 127.0.0.0/8 --syncmode full --port "$P2P_PORT" --maxpeers 4 --ipcpath "$IPC"
  --authrpc.addr 127.0.0.1 --authrpc.port "$((P2P_PORT + 1000))" --verbosity 5)

if [ ! -f "$DATADIR/genesis.json" ]; then
  echo pw > "$DATADIR/pw"
  "$GETH_BIN" --datadir "$DATADIR" account new --password "$DATADIR/pw" > /dev/null
  python3 -I - "$DATADIR" "$here/../../../xone-chainspec/genesis/devnet.json" <<'PY'
import glob, json, sys
d, src = sys.argv[1], sys.argv[2]
addr = json.load(open(glob.glob(d + "/keystore/*")[0]))["address"]
g = json.load(open(src))
# one signer: 32 bytes vanity, the signer address, 65 bytes seal
g["extraData"] = "0x" + "00" * 32 + addr + "00" * 65
json.dump(g, open(d + "/genesis.json", "w"))
open(d + "/signer.txt", "w").write("0x" + addr)
PY
  "$GETH_BIN" --datadir "$DATADIR" init "$DATADIR/genesis.json" > "$DATADIR/init.log" 2>&1
fi
SIGNER=$(cat "$DATADIR/signer.txt")
rm -f "$DATADIR/enode.txt"

wait_enode() {
  for _ in $(seq 1 60); do
    if enode=$(grep -o 'enode://[0-9a-f]*@[^ ?]*' "$DATADIR/geth.log" | head -1) && [ -n "$enode" ]; then
      echo "${enode%%@*}@127.0.0.1:$P2P_PORT" > "$DATADIR/enode.txt"
      return 0
    fi
    sleep 1
  done
  echo "geth did not report an enode; see $DATADIR/geth.log" >&2
  return 1
}
head_block() { "$GETH_BIN" attach --exec 'eth.blockNumber' "$IPC" 2>/dev/null | tail -1; }

if [ "$MODE" = prepare ]; then
  "$GETH_BIN" "${common[@]}" --mine --miner.etherbase "$SIGNER" --unlock "$SIGNER" \
    --password "$DATADIR/pw" --allow-insecure-unlock > "$DATADIR/geth.log" 2>&1 &
  pid=$!
  trap 'kill $pid ${hpid:-} 2>/dev/null || true' EXIT
  wait_enode
  : "${HOLDER:?set HOLDER to the command that runs hold_genesis_peer}"
  sleep 2
  gen=$("$GETH_BIN" attach --exec 'eth.getBlockByNumber(0).hash' "$IPC" | tr -d '"' | tail -1)
  XONE_GETH_ENODE=$(cat "$DATADIR/enode.txt") XONE_GETH_GENESIS=$gen bash -c "$HOLDER" &
  hpid=$!
  for _ in $(seq 1 360); do
    n=$(head_block || echo 0)
    [ "${n:-0}" -ge "$BLOCKS" ] 2>/dev/null && break
    sleep 2
  done
  echo "prepared: head $(head_block)"
  kill "$hpid" 2>/dev/null || true
  kill "$pid"; wait "$pid" 2>/dev/null || true
  exit 0
fi

"$GETH_BIN" "${common[@]}" > "$DATADIR/geth.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT
wait_enode
sleep 2
"$GETH_BIN" attach --exec 'eth.getBlockByNumber(0).hash' "$IPC" | tr -d '"' | tail -1 > "$DATADIR/genesis.txt"
cat "$DATADIR/enode.txt"
echo "genesis $(cat "$DATADIR/genesis.txt") head $(head_block)" >&2
wait "$pid"
