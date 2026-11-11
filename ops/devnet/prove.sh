#!/bin/bash
# The two-node loopback proof of docs/ops/first-mint-local.md: an xone producer (one throwaway key) and a geth-XDC
# follower, V1 into V2, with a kill -9 of the producer in a V1 slot and again in a V2 round.
#   prove.sh WORKDIR XONE_BIN GETH_BIN        (prints the transcript; WORKDIR is created)
# Loopback only, no public network, no real key.
set -u
W=$1; X=$2; G=$3; here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$W"; cd "$W"
t() { echo "[$(date +%T)] $*"; }
python3 "$here/gen.py" "$W/devnet" "$G" | sed 's/^/gen: /'
D=$W/devnet; mkdir -p geth xone; "$G" --datadir geth init "$D/genesis.json" >/dev/null 2>&1
xone() { nohup "$X" --datadir "$W/xone" --chainspec "$D/forks.json" --override.genesis "$D/genesis.json" --networkid 5551 --mine \
  --keystore "$D/keystore.json" --password "$D/password" --port 30311 --nodiscover --nat extip:127.0.0.1 --maxpeers 5 >> xone.log 2>&1 & echo $! > xone.pid; }
gx() { "$G" attach --exec "$1" "$W/geth/geth.ipc"; }
nohup "$G" --datadir geth --networkid 5551 --nodiscover --bootnodes "" --nat extip:127.0.0.1 --netrestrict 127.0.0.0/8 --syncmode full \
  --gcmode archive --port 30555 --maxpeers 4 --ipcpath "$W/geth/geth.ipc" --authrpc.port 8662 --verbosity 3 > geth.log 2>&1 & echo $! > geth.pid
t "start xone producer"; xone; sleep 6
PUB=$(cast wallet public-key --private-key 0x$(cat xone/nodekey) | tr -d '\n')
t "geth admin.addPeer(xone) -> $(gx "admin.addPeer('enode://${PUB#0x}@127.0.0.1:30311')")"
kill9() { # wait for a new sealed block, then kill -9 while the next one is being built, sealed or waited for
  local n0; n0=$(grep -c "sealed and sent" xone.log)
  while [ "$(grep -c 'sealed and sent' xone.log)" -le "$n0" ]; do sleep 0.05; done; sleep "$1"
  t "kill -9 xone (pid $(cat xone.pid)) $2"; kill -9 "$(cat xone.pid)"; sleep 1; xone; t "restarted xone (pid $(cat xone.pid))"; sleep 3
  gx "admin.addPeer('enode://${PUB#0x}@127.0.0.1:30311')" >/dev/null; }
until [ "$(gx 'eth.blockNumber')" -ge 12 ] 2>/dev/null; do sleep 1; done
t "head $(gx 'eth.blockNumber') (V1)"; kill9 "${V1_OFF:-0.9}" "in a V1 slot"
until [ "$(gx 'eth.blockNumber')" -ge 38 ] 2>/dev/null; do sleep 1; done
t "head $(gx 'eth.blockNumber') (V2)"; kill9 "${V2_OFF:-1.1}" "in a V2 round"
until [ "$(gx 'eth.blockNumber')" -ge 58 ] 2>/dev/null; do sleep 1; done
sleep 4
H=$(gx 'eth.blockNumber'); t "geth head $H"
echo "--- xone producer log (lines of interest)"; grep -E "validator|finishing|pending|committed block 1 |stopping|refused|not produced|dropp|sign transaction" xone.log | head -40
echo "--- every block number xone sealed, with the number of times: (must all be 1, or 2 when a restart finished a pending block)"
grep -oE "block [0-9]+ 0x[0-9a-f]+ sealed and sent" xone.log | awk '{print $2}' | sort -n | uniq -c | awk '{c[$1]++} END{for(k in c) print c[k], "heights sealed", k, "time(s)"}'
grep -oE "block [0-9]+ 0x[0-9a-f]+ sealed and sent" xone.log | awk '{print $2" "$3}' | sort -n | uniq | awk '{n[$1]++} END{for(k in n) if(n[k]>1) print "CONFLICT at height",k}'
echo "--- geth: head, V2 rounds, sign transactions in the chain"
gx "var o=[]; for (var i=1;i<=eth.blockNumber;i++){var b=eth.getBlock(i,true); b.transactions.forEach(function(t){o.push('block '+i+' to=0x..'+t.to.slice(-2)+' nonce='+t.nonce+' gas='+t.gas+' gasPrice='+t.gasPrice)})}; o.join('\n')"
gx "'geth head='+eth.blockNumber+' '+eth.getBlock(eth.blockNumber).hash"
echo "--- xone head"; grep -E "sealed and sent" xone.log | tail -1
echo "--- geth warnings about invalid blocks / bad blocks:"; grep -ciE "invalid block|bad block|rejected" geth.log
kill $(cat xone.pid) $(cat geth.pid) 2>/dev/null
