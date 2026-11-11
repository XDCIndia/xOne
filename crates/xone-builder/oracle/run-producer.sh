#!/bin/sh
# Prints the producer oracle (gas limit rows, sign transaction, timing rows) of the pinned geth-XDC as JSON.
#   GETH_SRC=<checkout at oracle/PIN> ./run-producer.sh > ../../../conformance/vectors/producer/oracle.json
# Same build as run.sh: the toolchain comes from the local module cache (GOPROXY=off) and the checkout is not written.
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
printf '{"Replace":{"%s/miner/zz_oracle_export.go":"%s/producer/export_miner.go.txt"}}' "$GETH_SRC" "$here" > "$w/overlay.json"
export GOWORK="$w/go.work" GOTOOLCHAIN="${GOTOOLCHAIN:-go1.25.7}" GOFLAGS= GOPROXY="${GOPROXY:-off}"
cd "$here"
go run -overlay "$w/overlay.json" ./producer "$@"
