#!/usr/bin/env bash
# Proves the ADR-002 deny list fires: for each banned crate a throwaway workspace that depends on a
# local crate of that name must fail `cargo deny check bans` and name it; a second revm must fail;
# a clean control must pass. Path crates only, so it runs offline.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
work=target/ci-deny-fixtures; rm -rf "$work"; mkdir -p "$work"
fail=0
mk() { # name dir version
  mkdir -p "$2/src"; touch "$2/src/lib.rs"
  printf '[package]\nname = "%s"\nversion = "%s"\nedition = "2021"\n\n[workspace]\n' "$1" "$3" > "$2/Cargo.toml"
}
run() { # case dir
  cargo deny --offline --config "$root/deny.toml" --manifest-path "$2/app/Cargo.toml" check bans 2>&1
}
for crate in $(sed -n 's/^    { crate = "\([^"]*\)", reason.*/\1/p' deny.toml); do
  d="$work/$crate"; mk "$crate" "$d/dep" 0.0.0; mk app "$d/app" 0.0.0
  printf '\n[dependencies]\n%s = { path = "../dep", version = "0.0.0" }\n' "$crate" >> "$d/app/Cargo.toml"
  if out=$(run "$crate" "$d") ; then echo "FAIL  $crate: banned crate passed"; fail=1
  elif grep -qF "$crate" <<<"$out"; then echo "ok    $crate (fails)"
  else echo "FAIL  $crate: failed without naming it"; echo "$out" | tail -5; fail=1; fi
done
# two revm versions
d="$work/revm-twice"; mk revm "$d/a" 1.0.0; mk revm "$d/b" 2.0.0; mk app "$d/app" 0.0.0
printf '\n[dependencies]\nrevm-a = { package = "revm", path = "../a", version = "1" }\nrevm-b = { package = "revm", path = "../b", version = "2" }\n' >> "$d/app/Cargo.toml"
if out=$(run revm "$d"); then echo "FAIL  two revm versions passed"; fail=1; else echo "ok    two revm versions (fails)"; fi
# control
d="$work/control"; mk fine "$d/dep" 0.0.0; mk app "$d/app" 0.0.0
printf '\n[dependencies]\nfine = { path = "../dep", version = "0.0.0" }\n' >> "$d/app/Cargo.toml"
if run control "$d" >/dev/null; then echo "ok    control (passes)"; else echo "FAIL  control failed"; run control "$d" | tail -5; fail=1; fi
[ "$fail" -eq 0 ] && echo "check-deny-fixtures: PASS" || { echo "check-deny-fixtures: FAIL"; exit 1; }
