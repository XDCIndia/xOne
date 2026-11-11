#!/bin/sh
# Prints the txpool scenarios and what the pinned geth-XDC legacy pool does on them, as JSON.
#   GETH_SRC=<checkout at oracle/PIN> ./run.sh > ../../../conformance/vectors/txpool/scenarios.json
# GOTOOLCHAIN: the pin needs go >= 1.25; the toolchain comes from the local module cache (GOPROXY=off).
# The overlay adds export.go.txt to package legacypool for the build only; the checkout is not written.
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
printf '{"Replace":{"%s/core/txpool/legacypool/zz_oracle_export.go":"%s/export.go.txt"}}' "$GETH_SRC" "$here" > "$w/overlay.json"
export GOWORK="$w/go.work" GOTOOLCHAIN="${GOTOOLCHAIN:-go1.25.7}" GOFLAGS= GOPROXY="${GOPROXY:-off}"
cd "$here"
go run -overlay "$w/overlay.json" . "$@"
