#!/usr/bin/env bash
# Release artifact check (XA-10): the shipped `xone` has no fault hooks, mutants, lab or
# test-only code, and carries a real version stamp.
#   (a) `cargo tree -p xone -e features` names no forbidden feature or test-only crate
#   (b) the release binary has no symbol or string from the test-only crates
#   (c) `xone --version` is not `unknown`, and has no `-dirty` on a clean tree
# Build with `-p xone` only: a whole-workspace build unifies `fault-injection` onto xone-db for sim.
# RELEASE_TREE_REQUIRE_CLEAN=1 (CI) fails on a dirty tree instead of expecting `-dirty`.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
say() { echo "$*"; }

forbidden='fault-injection|mutants|account-ext|pq-lab|pq_lab|xone-sim|xone-sweep|net-spike|wide-balance-spike'
tree=$(cargo tree --locked -p xone -e features --prefix none)
if hits=$(grep -nE "(feature \"($forbidden)\")|^($forbidden) v" <<<"$tree"); then
  say "FAIL (a): forbidden feature or crate in the release tree:"; say "$hits"; fail=1
else
  say "ok   (a) cargo tree -p xone -e features: no fault-injection, mutants, account-ext, lab or test-only crate"
fi

export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
cargo build --locked --release -p xone
bin=target/release/xone

syms='xone_sim|pq_lab|wide_balance_spike|net_spike|xone_sweep'
if hits=$( { nm -C "$bin" 2>/dev/null || true; strings -a "$bin"; } | grep -E "$syms" | sort -u | head -10) && [ -n "$hits" ]; then
  say "FAIL (b): test-only symbols in $bin:"; say "$hits"; fail=1
else
  say "ok   (b) nm and strings on $bin: none of $syms"
fi

stamp=$("$bin" --version)
say "     stamp: $stamp"
if [[ "$stamp" == *unknown* ]]; then say "FAIL (c): stamp contains 'unknown'"; fail=1; fi
if [ -z "$(git status --porcelain)" ]; then
  if [[ "$stamp" == *-dirty* ]]; then say "FAIL (c): clean tree but stamp is -dirty"; fail=1; else say "ok   (c) clean tree, stamp has no unknown and no -dirty"; fi
elif [ "${RELEASE_TREE_REQUIRE_CLEAN:-0}" = 1 ]; then
  say "FAIL (c): dirty tree"; fail=1
elif [[ "$stamp" == *-dirty* ]]; then
  say "note (c): tree is dirty and the stamp says -dirty; the clean-tree assertion was NOT exercised (commit, or set RELEASE_TREE_REQUIRE_CLEAN=1 in CI)"
else
  say "FAIL (c): dirty tree but stamp lacks -dirty"; fail=1
fi
[ "$fail" -eq 0 ] && say "check-release-tree: PASS" || { say "check-release-tree: FAIL"; exit 1; }
