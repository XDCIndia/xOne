#!/usr/bin/env bash
# Proves every R3 rule check fires on its fixture and that the real rule crates pass.
# Each ci/fixtures/<rule>/lib.rs starts with `// expect: <text>`; the fixture must
# fail `cargo clippy -D warnings` and the failure output must contain <text>.
set -euo pipefail
cd "$(dirname "$0")/.."
RULE_CRATES=(xone-chainspec xone-consensus xone-evm xone-state xone-primitives xone-signer xone-net xone-executor xone-sync xone-snap xone-builder xone-rpc)
fail=0

echo "== clippy.toml copies equal ci/rule2.clippy.toml"
for c in "${RULE_CRATES[@]}"; do
  cmp -s ci/rule2.clippy.toml "crates/$c/clippy.toml" || { echo "DRIFT: crates/$c/clippy.toml"; fail=1; }
  grep -qx 'workspace = true' <(sed -n '/^\[lints\]/,/^$/p' "crates/$c/Cargo.toml") || { echo "MISSING [lints] workspace = true: $c"; fail=1; }
done

echo "== fixtures must fail"
work=target/ci-fixtures
rm -rf "$work"; mkdir -p "$work"
for dir in ci/fixtures/*/; do
  rule=$(basename "$dir")
  [ -f "$dir/lib.rs" ] || continue
  want=$(sed -n '1s|^// expect: ||p' "$dir/lib.rs")
  d="$work/$rule"; mkdir -p "$d/src"
  cp "$dir/lib.rs" "$d/src/lib.rs"; cp ci/rule2.clippy.toml "$d/clippy.toml"
  # Same lint tables as the workspace: copied from the root manifest, one source of truth.
  { printf '[package]\nname = "fixture"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n\n[lints]\nworkspace = true\n\n[workspace]\n\n'
    sed -n '/^\[workspace.lints.rust\]/,$p' Cargo.toml; } > "$d/Cargo.toml"
  if out=$(cargo clippy --offline --manifest-path "$d/Cargo.toml" -- -D warnings 2>&1); then
    echo "FAIL  $rule: fixture compiled clean"; fail=1
  elif grep -qF -- "$want" <<<"$out"; then
    echo "ok    $rule (rejected: $want)"
  else
    echo "FAIL  $rule: failed, but not for '$want'"; echo "$out" | tail -15; fail=1
  fi
done

# Positive control: clean code passes under the same lints, so the failures above are the rules.
d="$work/_control"; cp -r "$work/as-conversions" "$d"
printf 'pub fn f(x: u8) -> u32 {\n    u32::from(x)\n}\n' > "$d/src/lib.rs"
if cargo clippy --offline --manifest-path "$d/Cargo.toml" -- -D warnings >/dev/null 2>&1; then echo "ok    control compiles clean"; else echo "FAIL  control"; fail=1; fi

echo "== grep fixtures must fail"
for f in ci/fixtures/globals-grep/*.rs; do
  if ci/check-globals.sh "$f" >/dev/null 2>&1; then echo "FAIL  $f not caught"; fail=1; else echo "ok    $(basename "$f")"; fi
done
# The same check must also reject the static-mut clippy fixture's declaration.
ci/check-globals.sh ci/fixtures/static-mut >/dev/null 2>&1 && { echo "FAIL  static-mut declaration not caught by grep"; fail=1; } || true

echo "== layering fixtures"
ci/check-layering.sh --fixtures || fail=1

echo "== real crates must pass"
ci/check-globals.sh && echo "ok    check-globals"
for c in "${RULE_CRATES[@]}"; do
  if cargo clippy --locked --all-targets -p "$c" -- -D warnings >/dev/null 2>&1; then echo "ok    $c"; else echo "FAIL  $c"; fail=1; fi
done
[ "$fail" -eq 0 ] && echo "check-fixtures: PASS" || { echo "check-fixtures: FAIL"; exit 1; }
