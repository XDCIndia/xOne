#!/usr/bin/env bash
# Test of ci/release.sh (XB-26), local only. In a temp clone: overlay the working tree (so an
# uncommitted change is tested), commit it as a throwaway commit, tag v<workspace version>, make a
# throwaway gpg key, then
#   1. refusals: dirty tree, no tag, tag that differs from the workspace version, no key
#   2. two builders: clone A and clone B (different paths, built in parallel) run ci/release.sh
#   3. checks on A: artifacts, SHA256SUMS verifies, signature verifies with the throwaway key and
#      fails on a changed file, provenance fields are filled, `xone --version` equals the tag stamp
#   4. A and B binaries and tarballs have the same SHA-256 (SOURCE_DATE_EPOCH = commit time)
# Nothing here is pushed, and no real key exists or is used. Cost: two release builds from nothing.
set -euo pipefail
src=$(cd "$(dirname "$0")/.." && pwd)
W=$(mktemp -d /tmp/xrel.XXXXXX)
[ "${KEEP:-0}" = 1 ] || trap 'rm -rf "$W"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok   $*"; }
G="git -c user.name=t -c user.email=t@example.invalid"

export GNUPGHOME=$W/gnupg; mkdir -m700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key throwaway-xone-release@example.invalid ed25519 sign never 2>/dev/null
KEY=$(gpg --list-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
export XONE_RELEASE_KEY=$KEY

git clone -q "$src" "$W/a"
(cd "$src" && git ls-files -co --exclude-standard -z) | tar -C "$src" --null -T - -cf - 2>/dev/null | tar -C "$W/a" -xf -
ver=$(python3 -I -c 'import sys,tomllib;print(tomllib.load(open(sys.argv[1],"rb"))["workspace"]["package"]["version"])' "$W/a/Cargo.toml")
cd "$W/a"
$G add -A && $G commit -qm "throwaway release test commit"

rel() { (cd "$1" && shift && env "$@" ci/release.sh 2>&1); }
out=$(rel "$W/a" XONE_RELEASE_KEY="$KEY" || true); [[ "$out" == *"not at a tag"* || "$out" == *"not exactly"* || "$out" == *"HEAD is not at a tag"* ]] || fail "untagged HEAD was not refused: $out"
ok "refuses HEAD without a tag"
$G tag "v9.9.9"
out=$(rel "$W/a" XONE_RELEASE_KEY="$KEY" || true); [[ "$out" == *"workspace version"* ]] || fail "tag/version mismatch was not refused: $out"
ok "refuses a tag that differs from the workspace version"
$G tag -d v9.9.9 >/dev/null; $G tag "v$ver"
out=$(rel "$W/a" XONE_RELEASE_KEY= || true); [[ "$out" == *"XONE_RELEASE_KEY"* ]] || fail "missing key was not refused: $out"
ok "refuses without XONE_RELEASE_KEY"
echo x > "$W/a/stray.txt"
out=$(rel "$W/a" XONE_RELEASE_KEY="$KEY" || true); [[ "$out" == *"dirty tree"* ]] || fail "dirty tree was not refused: $out"
rm "$W/a/stray.txt"
ok "refuses a dirty tree"

git clone -q "$W/a" "$W/b"; (cd "$W/b" && git fetch -q --tags origin && git checkout -q "v$ver")
(rel "$W/a" XONE_RELEASE_KEY="$KEY" > "$W/a.log") & pa=$!
(rel "$W/b" XONE_RELEASE_KEY="$KEY" > "$W/b.log") & pb=$!
wait $pa || { tail -20 "$W/a.log"; fail "release in A"; }
wait $pb || { tail -20 "$W/b.log"; fail "release in B"; }
ok "release.sh ran in two clones"

d=$W/a/dist; n=xone-$ver-linux-x86_64
for f in "$n.tar.gz" "xone-$ver.cdx.json" "xone-$ver.provenance.json" SHA256SUMS SHA256SUMS.sig; do [ -s "$d/$f" ] || fail "missing artifact $f"; done
ok "artifacts present: $(cd "$d" && echo *)"
(cd "$d" && sha256sum -c SHA256SUMS >/dev/null) || fail "checksums"
ok "SHA256SUMS verifies"
gpg --batch --verify "$d/SHA256SUMS.sig" "$d/SHA256SUMS" 2>/dev/null || fail "signature"
cp "$d/SHA256SUMS" "$W/tampered"; echo "00" >> "$W/tampered"
! gpg --batch --verify "$d/SHA256SUMS.sig" "$W/tampered" 2>/dev/null || fail "tampered sums verified"
ok "signature verifies with the throwaway key and fails on a changed file"

mkdir "$W/x"; tar -C "$W/x" -xzf "$d/$n.tar.gz"
want="xOneRS/v$ver-$(git -C "$W/a" rev-parse --short=8 HEAD)-$(date -u -d "@$(git -C "$W/a" log -1 --format=%ct)" +%d%m%Y)/linux-x86_64/rustc$(rustc --version | cut -d' ' -f2)"
got=$("$W/x/$n/xone" --version)
[ "$got" = "$want" ] || fail "stamp '$got' != '$want'"
ok "xone --version == $got"
python3 -I - "$d/xone-$ver.provenance.json" "$ver" "$(git -C "$W/a" rev-parse HEAD)" "$got" <<'PY' || fail "provenance"
import json, sys
p = json.load(open(sys.argv[1])); ver, commit, stamp = sys.argv[2:]
pr = p["predicate"]
assert p["predicateType"] == "https://slsa.dev/provenance/v1"
assert pr["runDetails"]["builder"]["id"]
assert pr["runDetails"]["metadata"]["startedOn"] and pr["runDetails"]["metadata"]["finishedOn"]
assert p["xone:commit"] == commit and p["xone:tag"] == "v" + ver and p["xone:stamp"] == stamp
assert all(s["digest"]["sha256"] and len(s["digest"]["sha256"]) == 64 for s in p["subject"]) and len(p["subject"]) == 2
deps = {d["uri"]: d["digest"] for d in pr["buildDefinition"]["resolvedDependencies"]}
assert len(deps["Cargo.lock"]["sha256"]) == 64 and len(deps["rust-toolchain.toml"]["sha256"]) == 64
assert p["xone:toolchain"].startswith("rustc 1.94.0") and pr["buildDefinition"]["internalParameters"]["SOURCE_DATE_EPOCH"]
print("     provenance warnings:", p["xone:warnings"], "| embedded dependency list:", p["xone:embeddedDependencyList"])
PY
ok "provenance fields filled"
python3 -I -c 'import json,sys;b=json.load(open(sys.argv[1]));assert b["bomFormat"]=="CycloneDX" and b["components"]' "$d/xone-$ver.cdx.json" || fail "sbom"
ok "SBOM is CycloneDX with components"

ha=$(sha256sum "$W/a/target/release/xone" | cut -d' ' -f1); hb=$(sha256sum "$W/b/target/release/xone" | cut -d' ' -f1)
ta=$(sha256sum "$d/$n.tar.gz" | cut -d' ' -f1); tb=$(sha256sum "$W/b/dist/$n.tar.gz" | cut -d' ' -f1)
echo "     binary A $ha"; echo "     binary B $hb"; echo "     tarball A $ta"; echo "     tarball B $tb"
if [ "$ha" != "$hb" ] || [ "$ta" != "$tb" ]; then
  objdump -h "$W/a/target/release/xone" > "$W/a.h"; objdump -h "$W/b/target/release/xone" > "$W/b.h"; diff "$W/a.h" "$W/b.h" | head -20
  fail "two clones differ"
fi
ok "two clones in different paths: identical binary and tarball SHA-256"
echo "test-release: PASS"
