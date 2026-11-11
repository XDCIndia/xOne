#!/usr/bin/env bash
# The chain for tests/live.rs: a private geth-XDC on 127.0.0.1 (the devnet genesis of xone-chainspec,
# one signer) with two accounts of 2^256-1 and one transfer from the signer to the other account, so
# that the receiver holds more than 2^256-1: a wide account in the state that snap serves. Then it
# serves the chain (crates/xone-net/tests/local-geth/run.sh, MODE=serve) in the foreground.
#
#   XONE_GETH_BIN  geth-XDC built from crates/xone-snap/oracle/PIN
#   DATADIR        new directory (default: a mktemp dir)
#   P2P_PORT       default 30499
#   BLOCKS         chain length (default 20)
# Run from the repository root. When it prints "genesis ... head N", run tests/live.rs as its
# header says, with DATADIR/enode.txt and DATADIR/genesis.txt.
set -euo pipefail
GETH_BIN=${XONE_GETH_BIN:?set XONE_GETH_BIN}
DATADIR=${DATADIR:-$(mktemp -d)}
export P2P_PORT=${P2P_PORT:-30499}
BLOCKS=${BLOCKS:-20}
run=crates/xone-net/tests/local-geth/run.sh
mkdir -p "$DATADIR"
echo pw > "$DATADIR/pw"
"$GETH_BIN" --datadir "$DATADIR" account new --password "$DATADIR/pw" > /dev/null 2>&1
python3 -I - "$DATADIR" crates/xone-chainspec/genesis/devnet.json <<'PY'
import glob, json, sys
d, src = sys.argv[1], sys.argv[2]
addr = json.load(open(glob.glob(d + "/keystore/*")[0]))["address"]
g = json.load(open(src))
g["extraData"] = "0x" + "00" * 32 + addr + "00" * 65
g["alloc"][addr] = {"balance": "0x" + "ff" * 32}
g["alloc"]["1111111111111111111111111111111111111111"] = {"balance": "0x" + "ff" * 32}
json.dump(g, open(d + "/genesis.json", "w"))
open(d + "/signer.txt", "w").write("0x" + addr)
PY
"$GETH_BIN" --datadir "$DATADIR" init "$DATADIR/genesis.json" > "$DATADIR/init.log" 2>&1
(
  # the account is unlocked a moment after the IPC socket opens: send until the node accepts it
  tx='eth.sendTransaction({from: eth.accounts[0], to: "0x1111111111111111111111111111111111111111", value: web3.toWei(1,"ether"), gasPrice: 1000000000000})'
  until out=$("$GETH_BIN" attach --exec "$tx" "$DATADIR/geth.ipc" 2>&1) && [[ $out == \"0x* ]]; do sleep 2; done
) &
XONE_GETH_BIN=$GETH_BIN DATADIR=$DATADIR MODE=prepare BLOCKS=$BLOCKS \
  HOLDER='cargo test --locked -p xone-net --test local_geth hold_genesis_peer -- --ignored --nocapture' \
  bash "$run"
wait
XONE_GETH_BIN=$GETH_BIN DATADIR=$DATADIR MODE=serve exec bash "$run"
