#!/bin/bash
# The Gate D rehearsal devnet of docs/ops/gate-d-rehearsal.md, on loopback only: three geth-XDC validators (g0, g1,
# g2), one xone validator (x) and one xone follower (f), each validator with its own throwaway key
# (ops/devnet/gen-gate-d.py; the keys stay under WORKDIR and are never committed).
#
#   gate-d.sh up      WORKDIR XONE_BIN GETH_BIN   make the chain and start all five nodes, full mesh
#   gate-d.sh start   WORKDIR NODE                start one node (g0 g1 g2 x f) on its datadir
#   gate-d.sh kill9   WORKDIR NODE                kill -9 one node
#   gate-d.sh stop    WORKDIR NODE                SIGTERM one node and wait for it
#   gate-d.sh mesh    WORKDIR                     every geth node adds every other node as a static peer
#   gate-d.sh cut     WORKDIR                     partition x: every node drops it (admin.removePeer); x has no
#                                                 static peers of its own, so nothing redials it
#   gate-d.sh heads   WORKDIR                     number and hash of the head of every running node
#   gate-d.sh turns   WORKDIR [FROM [TO]]         the turn table of x from chain data (ops/devnet/turns.py)
#   gate-d.sh signs   WORKDIR [FROM [TO]]         the 0x89 sign transactions of every sign block (ops/devnet/signs.py)
#   gate-d.sh record  WORKDIR                     the safety record of x (ops/devnet/record.py)
#   gate-d.sh sealkill WORKDIR                    kill -9 x in the seal-to-import window (ops/devnet/kill-at-seal.py)
#   gate-d.sh note    WORKDIR TEXT                a line in WORKDIR/events.log
#   gate-d.sh down    WORKDIR                     stop everything (the datadirs stay; rm -rf WORKDIR after)
# No public network: --nodiscover, --netrestrict 127.0.0.0/8, a private chain id (4747), loopback listeners.
set -u
cmd=$1; W=$2; here=$(cd "$(dirname "$0")" && pwd)
C=$W/chain
ts() { date +%T; }
stamp() { awk '{ print strftime("%T"), $0; fflush() }'; }   # xone logs carry no time of their own
log() { echo "[$(ts)] $*" | tee -a "$W/events.log"; }
mn() { python3 -c "import json;print(json.load(open('$C/chain.json'))['masternodes'][$1])"; }
keydir() { # the key directory of masternode index $1
  for d in "$C"/v*; do [ "$(cat "$d/address")" = "$(mn "$1")" ] && echo "$d"; done; }
# below the host's ephemeral range (15000-65000 here), so an outbound connection never holds a node's port
port() { case $1 in g0) echo 12561;; g1) echo 12562;; g2) echo 12563;; x) echo 12571;; f) echo 12572;; esac; }
# x is masternode 0: the genesis parent has only an in-turn sealer (index 0), and geth's validators do not seal before
# they imported a fresh block (eth/sync_xdc.go:2955-2975), so xone seals block 1. Block 30 (the V1 checkpoint, whose
# M2 tokens xone builds only for one masternode) falls to index 1 when every V1 slot is taken in turn.
idx() { case $1 in x) echo 0;; g0) echo 1;; g1) echo 2;; g2) echo 3;; esac; }
rpcport() { echo $(( $(port "$1") - 100 )); }   # HTTP RPC: g0 12461 .. x 12471, f 12472
# geth's IPC path would pass the 107-byte socket limit under a long WORKDIR: geth is reached over loopback HTTP
gx() { "$(cat "$W/geth.bin")" attach --exec "$2" "http://127.0.0.1:$(rpcport "$1")" 2>/dev/null; }
xr() { curl -s -m 5 -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" \
  "http://127.0.0.1:$(rpcport "$1")"; }
enode() {
  case $1 in
    g*) gx "$1" 'admin.nodeInfo.enode' | tr -d '"' | sed 's/@[^:]*:/@127.0.0.1:/; s/?.*//' ;;
    *) local pub; pub=$(cast wallet public-key --private-key "0x$(cat "$W/$1/nodekey")" | tr -d '\n')
       echo "enode://${pub#0x}@127.0.0.1:$(port "$1")" ;;
  esac; }

# the recorded pid of node $1, only if that process is still ours (its command line names this work directory):
# a stale pid file must never signal a reused pid. Never pkill/pgrep -f.
mypid() { local p; p=$(cat "$W/$1.pid" 2>/dev/null) || return 1
  tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | grep -qF -- "$W/" && echo "$p"; }

start() {
  local n=$1 G X; G=$(cat "$W/geth.bin"); X=$(cat "$W/xone.bin")
  # xone listens on 127.0.0.1 only when it has static peers and all are on loopback (services/net.rs); x and f
  # get one that nothing answers (port 12599), so neither dials a real node: the geth nodes dial them
  [ "$n" = x ] || [ "$n" = f ] && { mkdir -p "$W/$n"; [ -f "$W/$n/static-nodes.json" ] || echo \
    "[\"enode://$(printf '%0128d' 1)@127.0.0.1:12599\"]" > "$W/$n/static-nodes.json"; }
  case $n in
    g*) local k a; k=$(keydir "$(idx "$n")"); a=$(cat "$k/address")
      nohup "$G" --datadir "$W/$n" --networkid 4747 --nodiscover --bootnodes "" --nat extip:127.0.0.1 \
        --netrestrict 127.0.0.0/8 --syncmode full --gcmode full --port "$(port "$n")" --maxpeers 10 \
        --ipcdisable --http --http.addr 127.0.0.1 --http.port "$(rpcport "$n")" --http.api eth,net,admin,XDPoS,debug \
        --authrpc.port $(( $(port "$n") + 100 )) --verbosity 3 --allow-insecure-unlock \
        --mine --miner.etherbase "$a" --unlock "$a" --password "$k/password" >> "$W/$n.log" 2>&1 &
      echo $! > "$W/$n.pid" ;;
    x) local k; k=$(keydir 0)
      nohup "$X" --datadir "$W/x" --override.genesis "$C/genesis.json" --networkid 4747 --mine \
        --keystore "$k/keystore.json" --password "$k/password" --port 12571 --nodiscover --nat extip:127.0.0.1 \
        --maxpeers 10 --http --http.addr 127.0.0.1 --http.port 12471 --http.api eth,net,admin,XDPoS \
        --ipcdisable > >(stamp >> "$W/x.log") 2>&1 &
      echo $! > "$W/x.pid" ;;
    f) nohup "$X" --datadir "$W/f" --override.genesis "$C/genesis.json" --networkid 4747 --port 12572 \
        --nodiscover --nat extip:127.0.0.1 --maxpeers 10 --http --http.addr 127.0.0.1 --http.port 12472 \
        --http.api eth,net,admin,XDPoS --ipcdisable > >(stamp >> "$W/f.log") 2>&1 &
      echo $! > "$W/f.pid" ;;
  esac
  log "start $n pid $(cat "$W/$n.pid")"
}

mesh() { # geth peers are static on both ends' side that called addPeer: they redial after a restart of the other
  local -a e=(); for t in g0 g1 g2 x f; do e+=("$(enode $t)"); done
  local all=(g0 g1 g2 x f) i j
  for i in 0 1 2; do for j in 0 1 2 3 4; do [ "$i" = "$j" ] || gx "${all[$i]}" "admin.addPeer('${e[$j]}')" >/dev/null; done; done
}

case $cmd in
  up)
    X=$3; G=$4; mkdir -p "$W"; echo "$X" > "$W/xone.bin"; echo "$G" > "$W/geth.bin"
    python3 "$here/gen-gate-d.py" "$C" "$G" 4 | tee "$W/gen.out"
    for n in g0 g1 g2; do
      "$G" --datadir "$W/$n" init "$C/genesis.json" > /dev/null 2>&1
      mkdir -p "$W/$n/keystore"; cp "$(keydir "$(idx $n)")/keystore.json" "$W/$n/keystore/key.json"
    done
    for n in g0 g1 g2 x f; do start $n; done
    sleep 8; mesh; log "mesh up" ;;
  start) start "$3" ;;
  kill9) p=$(mypid "$3") && { log "kill -9 $3 pid $p"; kill -9 "$p"; } ;;
  stop) p=$(mypid "$3") && { log "stop $3 pid $p"; kill "$p"; while kill -0 "$p" 2>/dev/null; do sleep 0.2; done; } ;;
  mesh) mesh; log "mesh" ;;
  cut) e=$(enode x); for g in g0 g1 g2; do gx $g "admin.removePeer('$e')" >/dev/null; done
    xr f admin_removePeer "[\"$e\"]" >/dev/null; log "partition x: removed from g0 g1 g2 f" ;;
  heads)
    for g in g0 g1 g2; do mypid $g >/dev/null && echo "$g $(gx $g 'var b=eth.getBlock("latest"); b.number+" "+b.hash')"; done
    for n in x f; do mypid $n >/dev/null && echo "$n $(xr $n eth_getBlockByNumber '["latest",false]' | python3 -c 'import json,sys;b=json.load(sys.stdin)["result"];print(int(b["number"],16),b["hash"])')"; done ;;
  turns) python3 "$here/turns.py" "http://127.0.0.1:$(rpcport "${G:-g0}")" "$(mn 0)" "${3:-31}" "${4:-latest}" ;;
  signs) python3 "$here/signs.py" "http://127.0.0.1:$(rpcport g0)" "$(mn 0)" "${3:-1}" "${4:-latest}" ;;
  record) python3 "$here/record.py" "$W/x" ;;
  sealkill) p=$(mypid x) && log "kill -9 x in the seal window: $(python3 "$here/kill-at-seal.py" "$p" "$W/x" "$W/pending-$(date +%s).rlp")" ;;
  note) log "${*:3}" ;;
  down) for n in x f g0 g1 g2; do p=$(mypid $n) && kill "$p"; done; sleep 3; log "down" ;;
esac
