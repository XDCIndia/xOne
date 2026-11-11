#!/usr/bin/env bash
# Unsafe/FFI audit table of the `xone` release tree. Prints markdown on stdout:
#   ci/unsafe-audit.sh > docs/assurance/unsafe-ffi-audit-2026-11-11.md
# Method: cargo tree (exact package set) + cargo metadata (sources) + grep of `unsafe` in the
# vendored sources (ci/unsafe-audit.py). `cargo geiger` is not used: it re-parses sources with
# its own parser and lags the toolchain; run `cargo geiger -p xone` by hand as a cross-check.
set -euo pipefail
cd "$(dirname "$0")/.."
tree=$(mktemp); trap 'rm -f "$tree"' EXIT
cargo tree --locked -p xone -e normal,build --prefix none > "$tree"
cargo metadata --format-version 1 --locked --offline --filter-platform x86_64-unknown-linux-gnu |
  python3 -I ci/unsafe-audit.py "$tree"
