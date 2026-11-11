#!/usr/bin/env bash
# Live follow test: a loopback geth-XDC mines a private chain (crates/xone-net/tests/local-geth/run.sh,
# MODE=prepare) while the ignored test follows it as its only peer. The test is the "holder" that
# the XDC miner needs (a peer whose head is the genesis, eth/sync_xdc.go:812-838).
#
#   XONE_GETH_BIN  geth-XDC built from the oracle pin (conformance/vectors/net/oracle/PIN)
#   BLOCKS         the node stops mining at this height (default 80; the test follows 50)
#   OUT            where the test output goes (default: a new mktemp file)
#   DATADIR        the node's datadir; keep it between runs, the chain only grows
#   XONE_FOLLOW    head the test follows to (default 50)
#
# The pinned geth-XDC mines this private chain in bursts: its miner is gated while its syncer is busy,
# and the gate opens about two minutes after the first peer connects for 20 to 25 blocks (the same
# limit as `BLOCKS=20` in run.sh). A chain of more than one burst is built by running this again on
# the same DATADIR; each run then catches up to the existing chain and follows one more burst live.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=${OUT:-$(mktemp)}
cargo test --locked --offline -p xone-sync-live --test live --no-run 2>/dev/null
HOLDER="cargo test --locked --offline -p xone-sync-live --test live -- --ignored --nocapture --test-threads=1 > $OUT 2>&1"
MODE=prepare BLOCKS=${BLOCKS:-80} HOLDER="$HOLDER" bash crates/xone-net/tests/local-geth/run.sh
cat "$OUT"
