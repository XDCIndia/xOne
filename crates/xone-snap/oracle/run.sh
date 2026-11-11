#!/bin/sh
# Regenerates ../tests/vectors/snap.json from the geth-XDC oracle. GETH_SRC: checkout at oracle/PIN.
# Nothing in snap.json is edited by hand; crates/xone-snap/tests/vectors.rs reads it.
# Needs Go 1.25 first on PATH (GOTOOLCHAIN=local).
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
cd "$here"
mkdir -p ../tests/vectors
go run . > "$w/out.json"
mv "$w/out.json" ../tests/vectors/snap.json
