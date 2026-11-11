#!/usr/bin/env bash
# run-xone.sh: launch an xone node on a fleet server. Same env contract as the xOneGo
# ops/deploy/run-xone.sh, so /etc/xone/<net>.env files keep working.
#
#   NETWORK=apothem ETHSTATS_SECRET=... ops/run-xone.sh
#
# Env (all optional except NETWORK):
#   NETWORK         apothem | xdcmainnet | xdcdevnet | net2040
#                   (mainnet, xdc-mainnet, devnet and xdc-devnet are accepted as aliases)
#                   net2040 (chain 2040) needs GENESIS; anything else is a custom chain:
#                   set GENESIS and CHAINID
#   BOOTNODES       comma-separated enode URLs; default: ops/bootnodes/<net>.txt next to this script
#                   (XONE_BOOTNODES_DIR overrides the directory)
#   DATADIR         default /data/xone/$NETWORK
#   NODENAME        default xone-<net>-<short hostname>
#   ETHSTATS_HOST   default stats.xdcindia.com:443
#   ETHSTATS_SECRET no default. Without it the node starts WITHOUT --ethstats and is
#                   invisible on the stats board; the script says so.
#   P2P_PORT 30303, HTTP_PORT 8545, AUTH_PORT 8551, SYNCMODE full, MAXPEERS 50
#   GENESIS, CHAINID  custom chains only
#   XONE_BIN        default xone
#   EXTRA_ARGS      extra flags, split on spaces (e.g. "--mine --miner.etherbase 0x.. --password /path/pw")
set -euo pipefail

NETWORK="${NETWORK:?set NETWORK=apothem|xdcmainnet|xdcdevnet|net2040}"
DATADIR="${DATADIR:-/data/xone/$NETWORK}"
ETHSTATS_HOST="${ETHSTATS_HOST:-stats.xdcindia.com:443}"
ETHSTATS_SECRET="${ETHSTATS_SECRET:-}"
SYNCMODE="${SYNCMODE:-full}"
P2P_PORT="${P2P_PORT:-30303}"; HTTP_PORT="${HTTP_PORT:-8545}"; AUTH_PORT="${AUTH_PORT:-8551}"
MAXPEERS="${MAXPEERS:-50}"
XONE_BIN="${XONE_BIN:-xone}"
EXTRA_ARGS="${EXTRA_ARGS:-}"

net=(); NET=$NETWORK
case "$NETWORK" in
  mainnet|xdc-mainnet|xdcmainnet) NET=xdcmainnet; net=(--xdcmainnet) ;;
  apothem)                        net=(--apothem) ;;
  devnet|xdc-devnet|xdcdevnet)    NET=xdcdevnet; net=(--xdcdevnet) ;;
  net2040)
    : "${GENESIS:?net2040: set GENESIS=/path/net2040-genesis.json}"
    net=(--override.genesis "$GENESIS" --networkid 2040) ;;
  *)
    : "${GENESIS:?custom network $NETWORK: set GENESIS=/path/genesis.json}"
    : "${CHAINID:?custom network $NETWORK: set CHAINID=<chain id>}"
    net=(--override.genesis "$GENESIS" --networkid "$CHAINID") ;;
esac
NODENAME="${NODENAME:-xone-$NET-$(hostname -s 2>/dev/null || echo node)}"

# Bootnodes: the caller's BOOTNODES, else the committed list for the preset (comments skipped).
if [ -z "${BOOTNODES:-}" ]; then
  f="${XONE_BOOTNODES_DIR:-$(dirname "$0")/bootnodes}/$NET.txt"
  if [ -f "$f" ]; then BOOTNODES=$(grep -E '^enode://' "$f" | paste -sd, - || true); fi
fi
[ -n "${BOOTNODES:-}" ] || echo "run-xone: no bootnodes for $NET (set BOOTNODES); the node relies on discovery and static-nodes.json" >&2


args=("${net[@]}" --datadir "$DATADIR" --syncmode "$SYNCMODE" --identity "$NODENAME"
  --port "$P2P_PORT" --discovery.port "$P2P_PORT" --maxpeers "$MAXPEERS"
  --http --http.port "$HTTP_PORT" --authrpc.port "$AUTH_PORT")
[ -z "${BOOTNODES:-}" ] || args+=(--bootnodes "$BOOTNODES")
if [ -n "$ETHSTATS_SECRET" ]; then
  args+=(--ethstats "$NODENAME:$ETHSTATS_SECRET@$ETHSTATS_HOST")
else
  echo "run-xone: ETHSTATS_SECRET is not set; starting without --ethstats (node will not show on the stats board)" >&2
fi

# The JWT secret is created on first start, readable by the node user only.
if [ ! -f "$DATADIR/jwt.hex" ]; then
  mkdir -p "$DATADIR"
  (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$DATADIR/jwt.hex")
fi
args+=(--authrpc.jwtsecret "$DATADIR/jwt.hex")

echo "run-xone: network=$NETWORK node=$NODENAME datadir=$DATADIR" >&2
# shellcheck disable=SC2086  # EXTRA_ARGS is split on purpose, as in the xOneGo script
exec "$XONE_BIN" "${args[@]}" $EXTRA_ARGS
