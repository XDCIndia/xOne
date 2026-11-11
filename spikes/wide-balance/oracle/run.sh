#!/bin/sh
# Regenerates every vector file from the geth-XDC oracle. Nothing here is edited by hand.
# GETH_SRC: checkout of XDCIndia/go-ethereum at the commit in oracle/PIN.
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
cd "$here"
go run . encoding > ../vectors/encoding.json
cp ../vectors/encoding.json ../../../crates/xone-primitives/tests/vectors/wide_balance.json
go run . matrix > ../vectors/matrix.json
