#!/bin/sh
# Regenerates conformance/vectors/evm/*.json from the geth-XDC oracle. Nothing is edited by hand.
# GETH_SRC: checkout of XDCIndia/go-ethereum at the commit in conformance/PIN.
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
cd "$here"
pin=$(tr -d '[:space:]' < ../../../PIN)
go run . precompiles "$pin" > ../precompiles.json
go run . rules "$pin" > ../rules.json
go run . touch "$pin" > ../static_call_touch.json
