#!/bin/sh
# Populate conformance/geth-src with the geth-XDC commit named in conformance/PIN.
# The fork is private: this needs read access to it over ssh (or set GETH_XDC_URL).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
sha=$(tr -d '[:space:]' < "$here/PIN")
url=${GETH_XDC_URL:-git@github.com:XDCIndia/go-ethereum.git}
dir=$here/geth-src

mkdir -p "$dir"
cd "$dir"
[ -d .git ] || git init -q
git fetch -q --depth 1 "$url" "$sha"
git checkout -q --detach FETCH_HEAD
[ "$(git rev-parse HEAD)" = "$sha" ] || { echo "fetched commit is not $sha" >&2; exit 1; }
echo "conformance/geth-src is at $sha"
