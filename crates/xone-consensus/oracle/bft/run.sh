#!/bin/sh
# Prints the vectors of one command (timer, rule, forensics) from the pinned geth-XDC V2 engine.
#   GETH_SRC=<checkout at oracle/PIN> ./run.sh <command> > out.json
# The overlay adds export.go.txt to package engine_v2 and countdown.go.txt to package countdown for the
# build only; the checkout is not written. `timer` also reads the fork tables it is given as arguments:
#   ./run.sh timer crates/xone-chainspec/generated/forks.json conformance/vectors/consensus/net2040.forks.json
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
printf '{"Replace":{"%s/consensus/XDPoS/engines/engine_v2/zz_oracle_export.go":"%s/export.go.txt","%s/common/countdown/zz_oracle_export.go":"%s/countdown.go.txt"}}' "$GETH_SRC" "$here" "$GETH_SRC" "$here" > "$w/overlay.json"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
# arguments that are paths are made absolute: the build runs in the oracle directory
args=""
for a in "$@"; do
  case "$a" in
    *.json) a=$(cd "$(dirname "$a")" && pwd)/$(basename "$a") ;;
  esac
  args="$args $a"
done
cd "$here"
# shellcheck disable=SC2086
go run -overlay "$w/overlay.json" . $args
