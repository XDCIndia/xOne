#!/usr/bin/env bash
# Release rehearsal (XB-26) on a loopback geth-XDC devnet. systemd cannot be used without privileges, so
# the unit's job is done by ops/run-xone.sh, started from a temp /etc/xone-style env file
# (sourced with `set -a`, which is what EnvironmentFile= does for plain KEY=VALUE lines).
#
#   XONE_GETH_BIN=<geth-XDC from the pin>  XONE_HOLDER=<xone-net local_geth test binary> \
#   REL_N=<dir of unpacked release N>  REL_N1=<dir of unpacked release N+1>  ci/rehearse-release.sh
#
# Steps: (1) geth mines 16 blocks of a private chain (crates/xone-net/tests/local-geth/run.sh); (2) xone N follows it
# through the launcher; (3) SIGTERM, restart, same head; (4) the upgrade runbook: stop, copy the datadir,
# swap to N+1, start, head resumes; (5) the rollback runbook: stop, swap to N, start, head resumes;
# every start checks that the head block hash equals geth's. The live follow past new blocks is the xone-node live
# test (crates/xone-node/tests/live/run.sh); this geth stops mining after one burst. Loopback only; nothing touches a network.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
GETH=${XONE_GETH_BIN:?}; HOLDER=${XONE_HOLDER:?}; REL_N=${REL_N:?}; REL_N1=${REL_N1:?}
W=$(mktemp -d /tmp/xreh.XXXXXX); say() { echo "[rehearsal] $*"; }
pids=()
cleanup() { for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
free_port() { python3 -I -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1])"; }
GP=$(free_port); HP=$(free_port); G=$W/geth; IPC=$G/geth.ipc
rpc() { python3 -I "$here/crates/xone-node/tests/live/rpc.py" "$1" "$2" "$3" "${4:-[]}"; }
dec() { python3 -I -c "import sys;print(int(sys.argv[1],16))" "$1"; }
ghead() { dec "$(rpc ipc "$IPC" eth_blockNumber | tr -d '"')"; }
xhead() { dec "$(rpc http "http://127.0.0.1:$HP" eth_blockNumber | tr -d '"')"; }

say "geth: private devnet chain 5551, 16 blocks, loopback (a burst; this geth stops mining after about 17)"
XONE_GETH_BIN=$GETH DATADIR=$G P2P_PORT=$GP MODE=prepare BLOCKS=16 HOLDER="$HOLDER --ignored hold_genesis_peer --nocapture" \
  bash "$here/crates/xone-net/tests/local-geth/run.sh" 2>&1 | grep -E "prepared" || true
ENODE=$(cat "$G/enode.txt")
start_geth() {
  "$GETH" --datadir "$G" --networkid 5551 --nodiscover --bootnodes "" --nat extip:127.0.0.1 --netrestrict 127.0.0.0/8 \
    --syncmode full --port "$GP" --maxpeers 4 --ipcpath "$IPC" --authrpc.addr 127.0.0.1 --authrpc.port "$((GP + 1000))" \
    --verbosity 3 > "$W/geth.log" 2>&1 &
  GETH_PID=$!; pids+=("$GETH_PID")
  for _ in $(seq 1 60); do [ -S "$IPC" ] && ghead >/dev/null 2>&1 && return 0; sleep 1; done; echo "geth did not start" >&2; exit 1
}
start_geth
say "geth serves head $(ghead), enode $ENODE"

# the temp /etc/xone
mkdir -p "$W/etc/xone" "$W/opt"
cat > "$W/etc/xone/xdcdevnet.env" <<ENV
DATADIR=$W/data/xdcdevnet
BOOTNODES=$ENODE
P2P_PORT=0
HTTP_PORT=$HP
AUTH_PORT=0
EXTRA_ARGS="--override.genesis $G/genesis.json --nodiscover --http.api eth,net,web3,XDPoS"
ENV
chmod 600 "$W/etc/xone/xdcdevnet.env"
use_release() { rm -rf "$W/opt/xone"; cp -r "$1" "$W/opt/xone"; }
start_xone() { # logs to $W/xone.<n>.log
  XN=$((${XN:-0} + 1))
  ( set -a
    # shellcheck source=/dev/null
    . "$W/etc/xone/xdcdevnet.env"; set +a
    NETWORK=xdcdevnet XONE_BIN="$W/opt/xone/xone" XONE_BOOTNODES_DIR="$W/opt/xone/bootnodes" exec "$W/opt/xone/run-xone.sh" ) > "$W/xone.$XN.log" 2>&1 &
  XPID=$!; pids+=("$XPID")
  for _ in $(seq 1 60); do rpc http "http://127.0.0.1:$HP" web3_clientVersion >/dev/null 2>&1 && return 0; sleep 1; done
  echo "xone did not start" >&2; cat "$W/xone.$XN.log" >&2; exit 1
}
stop_xone() {
  local t0 t1; t0=$(date +%s.%N); kill -TERM "$XPID"
  while kill -0 "$XPID" 2>/dev/null; do sleep 0.1; done
  t1=$(date +%s.%N); wait "$XPID" && code=0 || code=$?
  say "SIGTERM: stopped in $(python3 -I -c "print(round($t1 - $t0, 2))")s, exit code $code"
  [ "$code" = 0 ] || { echo "FAIL: exit code $code" >&2; exit 1; }
}
caught_up() { for _ in $(seq 1 90); do [ "$(xhead)" -ge "$1" ] && return 0; sleep 1; done; echo "FAIL: xone head $(xhead) < $1" >&2; exit 1; }
check_stamp() {
  local cv; cv=$(rpc http "http://127.0.0.1:$HP" web3_clientVersion | tr -d '"')
  [ "$cv" = "$("$W/opt/xone/xone" --version)" ] || { echo "FAIL: web3_clientVersion $cv" >&2; exit 1; }
  say "web3_clientVersion = $cv (equals xone --version)"
  local hx hg; hx=$(rpc http "http://127.0.0.1:$HP" eth_getBlockByNumber '["latest",false]' | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["hash"])')
  hg=$(rpc ipc "$IPC" eth_getBlockByNumber "[\"0x$(printf '%x' "$GH")\",false]" | python3 -I -c 'import json,sys;print(json.load(sys.stdin)["hash"])')
  [ "$hx" = "$hg" ] || { echo "FAIL: head hash $hx != geth $hg" >&2; exit 1; }
  say "head block hash $hx equals geth's"
}
GH=$(ghead)

say "1. release N under the launcher"; use_release "$REL_N"; start_xone; caught_up "$GH"
grep -E "run-xone:|following from" "$W/xone.1.log" | sed 's/^/[xone] /'; check_stamp
H1=$(xhead); say "head $H1 (geth $GH), jwt.hex: $(stat -c '%a %s bytes' "$W/data/xdcdevnet/jwt.hex")"
say "2. SIGTERM and restart"; stop_xone; start_xone; H2=$(xhead)
say "restart head $H2 (before: $H1)"; [ "$H2" = "$H1" ] || { echo "FAIL: head after restart" >&2; exit 1; }
say "3. upgrade N -> N+1 (ops/upgrade-rollback.md section A)"
stop_xone; cp -a "$W/data/xdcdevnet" "$W/data/xdcdevnet.pre-N1"
use_release "$REL_N1"; start_xone; H3=$(xhead); check_stamp
say "head after upgrade $H3 (before: $H1)"; [ "$H3" -ge "$H1" ] || { echo "FAIL: head went back" >&2; exit 1; }
say "4. rollback N+1 -> N, format unchanged (section B2)"
stop_xone; use_release "$REL_N"; start_xone; H4=$(xhead); check_stamp
say "head after rollback $H4"; [ "$H4" -ge "$H3" ] || { echo "FAIL: head went back" >&2; exit 1; }
say "5. rollback by restoring the pre-upgrade copy (section B3)"
stop_xone; mv "$W/data/xdcdevnet" "$W/data/xdcdevnet.failed"; cp -a "$W/data/xdcdevnet.pre-N1" "$W/data/xdcdevnet"
start_xone; H5=$(xhead); check_stamp; say "head after restore $H5 (backup was taken at $H1)"; [ "$H5" = "$H1" ] || { echo "FAIL: restore head" >&2; exit 1; }
stop_xone
say "PASS; work dir $W"
