#!/usr/bin/env bash
# R3 rule 2, the part Clippy 1.94 cannot express: `static mut` declarations and
# lazy_static / once_cell globals. (`static mut` use needs `unsafe`, which the
# workspace lint `unsafe_code = "forbid"` rejects; a bare declaration compiles, so grep.)
# Usage: ci/check-globals.sh [path...]   default: the rule-2 crates. Exit 1 on a match.
set -u
cd "$(dirname "$0")/.."
if [ $# -eq 0 ]; then
  set -- crates/xone-chainspec crates/xone-consensus crates/xone-evm crates/xone-state crates/xone-primitives crates/xone-signer crates/xone-net crates/xone-executor crates/xone-sync crates/xone-snap crates/xone-builder crates/xone-rpc
fi
status=0
grep -rnE --include='*.rs' '^[^/]*(\bstatic[[:space:]]+mut\b|\blazy_static\b|\bonce_cell\b)' "$@" || status=$?
case "$status" in
  0)
    echo "check-globals: forbidden global state (R3 rule 2)" >&2
    exit 1
    ;;
  1) exit 0 ;; # grep completed and found no forbidden declarations.
  *)
    echo "check-globals: scan failed (grep exit $status); no qualification result" >&2
    exit "$status"
    ;;
esac

