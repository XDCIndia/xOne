#!/usr/bin/env bash
# Layering and crate allow-list check (plan #1 Layout). Rules live in ci/layering.py.
#   ci/check-layering.sh             check this workspace
#   ci/check-layering.sh --fixtures  prove each ci/fixtures/layering case is judged as declared
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${1:-}" != "--fixtures" ]; then
  cargo metadata --format-version 1 --no-deps --locked --offline | python3 -I ci/layering.py
  echo "check-layering: PASS"
  exit 0
fi
work=target/ci-layering
rm -rf "$work"; mkdir -p "$work"
# Generate one tiny workspace per case. Package name = last path component, except
# the test-only and bin crates, whose names are given by the table below.
python3 -I - "$work" <<'PY'
import json, os, sys
NAMES = {"sim": "xone-sim", "labs/pq": "pq-lab", "bin/xone": "xone"}
work = sys.argv[1]
for case, spec in json.load(open("ci/fixtures/layering/cases.json")).items():
    if case.startswith("_"):
        continue
    base = os.path.join(work, case)
    os.makedirs(base)
    open(os.path.join(base, "Cargo.toml"), "w").write(
        "[workspace]\nresolver = \"2\"\nmembers = %s\n" % json.dumps(list(spec["crates"])))
    for d, deps in spec["crates"].items():
        os.makedirs(os.path.join(base, d, "src"))
        open(os.path.join(base, d, "src/lib.rs"), "w").write("")
        body = '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\n' % NAMES.get(d, os.path.basename(d))
        for dep in deps:
            path = next(k for k in list(spec["crates"]) + list(NAMES) if NAMES.get(k, os.path.basename(k)) == dep)
            body += '%s = { path = "%s" }\n' % (dep, os.path.relpath(os.path.join(base, path), os.path.join(base, d)))
        open(os.path.join(base, d, "Cargo.toml"), "w").write(body)
PY
fail=0
for dir in "$work"/*/; do
  case=$(basename "$dir")
  want=$(python3 -I -c 'import json,sys;print(json.load(open("ci/fixtures/layering/cases.json"))[sys.argv[1]]["ok"])' "$case")
  if out=$(cargo metadata --format-version 1 --no-deps --offline --manifest-path "$dir/Cargo.toml" | LAYERING_NO_EXCEPTIONS=1 python3 -I ci/layering.py 2>&1); then got=True; else got=False; fi
  if [ "$got" = "$want" ]; then echo "ok    $case (ok=$got)"; else echo "FAIL  $case: expected ok=$want"; echo "$out"; fail=1; fi
done
exit $fail
