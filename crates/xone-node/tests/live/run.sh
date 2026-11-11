#!/usr/bin/env bash
# First follow (docs/ops/first-follow.md): xone follows a private geth-XDC chain on loopback.
#
#   1. crates/xone-net/tests/local-geth/run.sh MODE=prepare mines PREPARE blocks (devnet 5551,
#      one signer, loopback only, no discovery).
#   2. geth mines on with the same flags while xone connects, imports the chain, and follows
#      FOLLOW more blocks with a head lag of at most 2. RPC answers are compared with geth's.
#   3. SIGTERM stops xone within 5 s; a restart resumes at the same head.
#
# Needs: XONE_GETH_BIN (geth-XDC built from the pin), XONE_BIN (target/debug/xone),
# XONE_HOLDER (the xone-net local_geth test binary), a short WORK path (unix socket limit).
# Loopback only: nothing here touches a public network.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
GETH=${XONE_GETH_BIN:?set XONE_GETH_BIN}
XONE=${XONE_BIN:-$root/target/debug/xone}
HOLDER=${XONE_HOLDER:?set XONE_HOLDER to the local_geth test binary}
WORK=${WORK:-$(mktemp -d /tmp/xone-live.XXXXXX)}
PREPARE=${PREPARE:-12}
FOLLOW=${FOLLOW:-50}
free_port() { python3 -I -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1])"; }
GP=${P2P_PORT:-$(free_port)}
HP=${HTTP_PORT:-$(free_port)}
G=$WORK/geth
IPC=$G/geth.ipc
say() { echo "[live] $*"; }
rpc_ipc() { python3 -I "$here/rpc.py" ipc "$IPC" "$1" "${2:-[]}"; }
rpc_http() { python3 -I "$here/rpc.py" http "http://127.0.0.1:$HP" "$1" "${2:-[]}"; }
hex2dec() { python3 -I -c "import sys;print(int(sys.argv[1],16))" "$1"; }
geth_head() { hex2dec "$(rpc_ipc eth_blockNumber | tr -d '"')"; }
xone_head() { hex2dec "$(rpc_http eth_blockNumber | tr -d '"')"; }
pids=()
cleanup() { for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

say "work dir $WORK, geth p2p port $GP, xone http port $HP"
say "phase 1: run.sh MODE=prepare, $PREPARE blocks"
XONE_GETH_BIN=$GETH DATADIR=$G P2P_PORT=$GP MODE=prepare BLOCKS=$PREPARE HOLDER="$HOLDER --ignored hold_genesis_peer --nocapture" \
  bash "$root/crates/xone-net/tests/local-geth/run.sh" 2>&1 | grep -E "prepared|enode" || true
SIGNER=$(cat "$G/signer.txt")
ENODE=$(cat "$G/enode.txt")
say "prepared chain, enode $ENODE"

start_geth() {
  "$GETH" --datadir "$G" --networkid 5551 --nodiscover --bootnodes "" --nat extip:127.0.0.1 \
    --netrestrict 127.0.0.0/8 --syncmode full --port "$GP" --maxpeers 4 --ipcpath "$IPC" \
    --authrpc.addr 127.0.0.1 --authrpc.port "$((GP + 1000))" --verbosity 5 \
    --mine --miner.etherbase "$SIGNER" --unlock "$SIGNER" --password "$G/pw" --allow-insecure-unlock \
    > "$G/geth2.log" 2>&1 &
  GETH_PID=$!
  pids+=("$GETH_PID")
  for _ in $(seq 1 60); do [ -S "$IPC" ] && geth_head >/dev/null 2>&1 && return 0; sleep 1; done
  echo "geth did not come up" >&2; exit 1
}
say "phase 2: geth restarted with the same flags and --mine"
start_geth
say "geth head at restart: $(geth_head)"

start_xone() {
  "$XONE" --xdcdevnet --override.genesis "$G/genesis.json" --datadir "$WORK/xone" --bootnodes "$ENODE" \
    --nodiscover --port 0 --http --http.port "$HP" --http.api eth,net,web3,XDPoS "$@" \
    > "$WORK/xone.$RUN.log" 2>&1 &
  XONE_PID=$!
  pids+=("$XONE_PID")
  for _ in $(seq 1 60); do rpc_http web3_clientVersion >/dev/null 2>&1 && return 0; sleep 1; done
  echo "xone did not come up" >&2; cat "$WORK/xone.$RUN.log" >&2; exit 1
}
RUN=1
start_xone
say "xone started: $(grep -m1 'following from' "$WORK/xone.1.log")"
for _ in $(seq 1 90); do
  g=$(geth_head); x=$(xone_head)
  [ "$x" -ge 1 ] && [ $((g - x)) -le 2 ] && break
  sleep 1
done
say "connected and imported: xone head $x, geth head $g"
grep -E "connected|imported" "$WORK/xone.1.log" | head -5 | sed 's/^/[xone] /'

say "following $FOLLOW more blocks, head lag sampled every second"
# This geth mines in bursts of about 17 blocks and then stops until it is restarted (its miner is gated
# by its own syncer, whatever the peer does). A stalled geth is restarted; xone redials it by itself.
start=$(geth_head); max=0; samples=0; last=$start; last_t=$(date +%s); restarts=0; mined=0
while :; do
  g=$(geth_head 2>/dev/null) || { sleep 1; continue; }
  x=$(xone_head)
  lag=$((g - x)); [ "$lag" -gt "$max" ] && max=$lag; samples=$((samples + 1))
  [ "$g" -ge $((start + FOLLOW)) ] && break
  now=$(date +%s)
  if [ "$g" != "$last" ]; then last=$g; last_t=$now; mined=1; fi
  if [ "$mined" = 1 ] && [ $((now - last_t)) -ge 45 ]; then
    say "  geth stalled at $g, restarting it (restart $((restarts + 1)))"
    kill "$GETH_PID"; wait "$GETH_PID" 2>/dev/null || true
    start_geth; restarts=$((restarts + 1)); mined=0; last_t=$(date +%s)
  fi
  sleep 1
done
say "geth mined $((g - start)) blocks (head $start -> $g, $restarts restarts), $samples samples, max head lag $max"
[ "$max" -le 2 ] || { echo "FAIL: head lag $max > 2"; exit 1; }
for _ in $(seq 1 20); do [ "$(geth_head)" = "$(xone_head)" ] && break; sleep 1; done

B=$(printf '0x%x' $(( $(xone_head) - 1 )))
say "RPC answers at block $B (xone over HTTP, geth over IPC)"
fail=0
cmp_rpc() {
  local m=$1 p=$2 a b
  a=$(rpc_http "$m" "$p"); b=$(rpc_ipc "$m" "$p")
  if [ "$a" = "$b" ]; then say "  $m $p: equal"; else say "  $m $p: DIFFERENT"; echo "xone: $a"; echo "geth: $b"; fail=1; fi
}
say "  eth_blockNumber xone $(rpc_http eth_blockNumber) geth $(rpc_ipc eth_blockNumber)"
cmp_rpc eth_getBlockByNumber "[\"$B\", false]"
cmp_rpc eth_getBlockByNumber "[\"0x0\", true]"
cmp_rpc XDPoS_getMasternodesByNumber "[\"$B\"]"
cv=$(rpc_http web3_clientVersion | tr -d '"'); want=$("$XONE" --version)
[ "$cv" = "$want" ] && say "  web3_clientVersion = $cv (equals xone --version)" || { say "  web3_clientVersion $cv differs from $want"; fail=1; }
[ "$fail" = 0 ] || { echo "FAIL: RPC answers differ"; exit 1; }

say "phase 3: geth stops mining, SIGTERM, restart"
kill "$GETH_PID"; wait "$GETH_PID" 2>/dev/null || true
sleep 3
h1=$(xone_head)
t0=$(date +%s.%N)
kill -TERM "$XONE_PID"
while kill -0 "$XONE_PID" 2>/dev/null; do sleep 0.1; done
t1=$(date +%s.%N)
took=$(python3 -I -c "print(round($t1 - $t0, 2))")
wait "$XONE_PID" && code=0 || code=$?
say "SIGTERM: stopped in ${took}s, exit code $code, head $h1"
python3 -I -c "import sys;sys.exit(0 if $took <= 5 and $code == 0 else 1)" || { echo "FAIL: stop took too long or exited with $code"; exit 1; }
tail -3 "$WORK/xone.1.log" | sed 's/^/[xone] /'
RUN=2
start_xone
say "restart: $(grep -m1 'following from' "$WORK/xone.2.log")"
h2=$(xone_head)
say "restart head $h2 (before the stop: $h1)"
[ "$h2" = "$h1" ] && [ "$h2" -gt 0 ] || { echo "FAIL: restart head $h2, expected $h1"; exit 1; }
kill -TERM "$XONE_PID"; wait "$XONE_PID" || true
say "PASS"
