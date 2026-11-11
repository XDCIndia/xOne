#!/bin/sh
# Prints the reward map of each case of a JSON file, from the pinned geth-XDC reward functions.
#   GETH_SRC=<checkout at oracle/PIN> ./run.sh cases.json [net2040.forks.json] > rewards.json
set -eu
: "${GETH_SRC:?set GETH_SRC to the geth-XDC checkout}"
here=$(cd "$(dirname "$0")" && pwd)
w=$(mktemp -d)
trap 'rm -rf "$w"' EXIT
printf 'go 1.25.0\n\nuse (\n\t%s\n\t%s\n)\n' "$here" "$GETH_SRC" > "$w/go.work"
export GOWORK="$w/go.work" GOTOOLCHAIN=local GOFLAGS=
if [ "$1" = floats ]; then shift; cd "$here"; exec go run . floats "$@"; fi
in=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
forks=${2:+$(cd "$(dirname "$2")" && pwd)/$(basename "$2")}
cd "$here"
go run . "$in" ${forks:+"$forks"}
