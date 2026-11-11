#!/bin/sh
# Prints the vectors of one command (sort, calc, epoch, cert, commit, midadd) from the pinned geth-XDC V2 engine.
#   GETH_SRC=<checkout at oracle/PIN> ./run.sh <command> > out.json
# The overlay adds export.go.txt to package engine_v2 for the build only; the checkout is not written.
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
printf '{"Replace":{"%s/consensus/XDPoS/engines/engine_v2/zz_oracle_export.go":"%s/export.go.txt"}}' "$GETH_SRC" "$here" > "$w/overlay.json"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
cd "$here"
go run -overlay "$w/overlay.json" . "$@"
