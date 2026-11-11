#!/usr/bin/env bash
# Release build (XB-26). Run from a clean checkout of a tag on main:
#
#   XONE_RELEASE_KEY=<gpg key id> [GNUPGHOME=<keyring>] ci/release.sh
#
# Writes dist/ (override with XONE_DIST):
#   xone-<version>-linux-x86_64.tar.gz   xone, run-xone.sh, xone@.service, bootnodes/, LICENSE
#   xone-<version>.cdx.json              SBOM (CycloneDX 1.5); cargo tree manifest if cargo-cyclonedx is missing
#   xone-<version>.provenance.json       in-toto statement, SLSA provenance v1 shaped
#   SHA256SUMS                           over the three files above
#   SHA256SUMS.sig                       detached gpg signature of SHA256SUMS
#
# Refuses: a dirty tree, HEAD that is not exactly a tag, a tag that differs from v<workspace version>,
# a stamp that says `unknown` or `-dirty`, or a missing signing key. The release key is never in this
# repository; the owner decides its custody (docs/ops/release.md).
# Tools: cargo-auditable 0.7.7 and cargo-cyclonedx 0.5.10 are installed with `--locked` when missing.
# If an install fails the build goes on without it and the gap is recorded under
# `xone:warnings` in the provenance file.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
die() { echo "release: $*" >&2; exit 1; }
warns=()
warn() { echo "release: WARNING: $*" >&2; warns+=("$*"); }

KEY="${XONE_RELEASE_KEY:-}"
[ -n "$KEY" ] || die "set XONE_RELEASE_KEY to the gpg key id that signs SHA256SUMS (no default, no key in the repo)"
[ -z "$(git status --porcelain)" ] || die "dirty tree"
TAG=$(git describe --exact-match --tags HEAD 2>/dev/null) || die "HEAD is not at a tag"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || die "tag $TAG is not vMAJOR.MINOR.PATCH"
VERSION=${TAG#v}
ws=$(python3 -I -c 'import sys,tomllib;print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
[ "$ws" = "$VERSION" ] || die "tag $TAG but the workspace version is $ws (bump it in a PR before tagging)"
COMMIT=$(git rev-parse HEAD)
SDE=$(git log -1 --format=%ct)
export SOURCE_DATE_EPOCH=$SDE
DIST="${XONE_DIST:-$root/dist}"
rm -rf "$DIST"; mkdir -p "$DIST"
STARTED=$(date -u -d "@$(date +%s)" +%Y-%m-%dT%H:%M:%SZ)

# tools: pinned, installed with --locked, failure is recorded not fatal
need() { # need <binary> <crate> <version>
  command -v "$1" >/dev/null 2>&1 && return 0
  cargo install "$2" --locked --version "$3" >/dev/null 2>&1 && return 0
  return 1
}
auditable=1; need cargo-auditable cargo-auditable 0.7.7 || { auditable=0; warn "cargo-auditable not installable: plain 'cargo build --release --locked'; the binary has no embedded dependency list"; }
cyclone=1;   need cargo-cyclonedx cargo-cyclonedx 0.5.10 || { cyclone=0; warn "cargo-cyclonedx not installable: SBOM is a 'cargo tree --locked' text manifest, not CycloneDX"; }

# Same bytes from any checkout path or home directory: strip build paths and the build id.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
sysroot=$(rustc --print sysroot)
export RUSTFLAGS="--remap-path-prefix=$root=/build --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$sysroot=/rustc -C link-arg=-Wl,--build-id=none"
if [ "$auditable" = 1 ]; then
  cargo auditable build --release --locked -p xone
else
  cargo build --release --locked -p xone
fi
bin=target/release/xone

stamp=$("$bin" --version)
[[ "$stamp" != *unknown* ]] || die "stamp contains 'unknown': $stamp"
[[ "$stamp" != *-dirty* ]] || die "stamp says -dirty: $stamp"
want="xOne/v$VERSION-${COMMIT:0:8}-$(date -u -d "@$SDE" +%d%m%Y)/linux-x86_64/rustc$(rustc --version | cut -d' ' -f2)"
[ "$stamp" = "$want" ] || die "stamp mismatch: binary says '$stamp', tag and commit say '$want'"
has_dep=no; objdump -h "$bin" | grep -q '\.dep-v0' && has_dep=yes
[ "$auditable" = 0 ] || [ "$has_dep" = yes ] || die "built with cargo-auditable but the binary has no .dep-v0 section"

# tarball: fixed order, owner, mtime and gzip header
name=xone-$VERSION-linux-x86_64
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name"
install -m755 "$bin" "$stage/$name/xone"
install -m755 ops/run-xone.sh "$stage/$name/run-xone.sh"
install -m644 ops/xone@.service "$stage/$name/xone@.service"
cp -r ops/bootnodes "$stage/$name/bootnodes"
install -m644 LICENSE "$stage/$name/LICENSE"
tar -C "$stage" --sort=name --mtime="@$SDE" --owner=0 --group=0 --numeric-owner -cf - "$name" | gzip -n -9 > "$DIST/$name.tar.gz"

# SBOM
sbom=xone-$VERSION.cdx.json
if [ "$cyclone" = 1 ]; then
  cargo cyclonedx --manifest-path bin/xone/Cargo.toml --format json --spec-version 1.5 --override-filename "xone-sbom" >/dev/null
  mv bin/xone/xone-sbom.json "$DIST/$sbom"
  git ls-files -o --exclude-standard | grep -E 'xone-sbom|\.cdx\.json$' | xargs -r rm -f
else
  sbom=xone-$VERSION.cargo-tree.txt
  cargo tree --locked -p xone -e normal --prefix none | sort -u > "$DIST/$sbom"
fi
[ -z "$(git status --porcelain)" ] || die "the build dirtied the tree: $(git status --porcelain | head -3)"

# provenance (in-toto Statement v1, SLSA provenance v1 predicate shape)
FINISHED=$(date -u +%Y-%m-%dT%H:%M:%SZ)
export TAG COMMIT SDE STARTED FINISHED VERSION name sbom stamp has_dep RUSTFLAGS
export DIST BUILDER="${XONE_BUILDER_ID:-local:ci/release.sh}"
REPO_URI=$(git remote get-url origin 2>/dev/null | sed -E 's#//[^/@]*@#//#' || true)
RUSTC_VV=$(rustc -vV); CARGO_V=$(cargo --version); AUDITABLE_V=$(cargo auditable --version 2>/dev/null || echo none)
WARNINGS=$(printf '%s\n' "${warns[@]:-}")
CARGO_LOCK_SHA=$(sha256sum Cargo.lock | cut -d' ' -f1); TOOLCHAIN_SHA=$(sha256sum rust-toolchain.toml | cut -d' ' -f1)
BUILD_SCRIPT_SHA=$(sha256sum ci/release.sh | cut -d' ' -f1)
export REPO_URI RUSTC_VV CARGO_V AUDITABLE_V WARNINGS CARGO_LOCK_SHA TOOLCHAIN_SHA BUILD_SCRIPT_SHA
python3 -I - <<'PY' > "$DIST/xone-$VERSION.provenance.json"
import hashlib, json, os
e = os.environ
def sha(f): return hashlib.sha256(open(os.path.join(e["DIST"], f), "rb").read()).hexdigest()
subjects = [{"name": f, "digest": {"sha256": sha(f)}} for f in (e["name"] + ".tar.gz", e["sbom"])]
print(json.dumps({
  "_type": "https://in-toto.io/Statement/v1",
  "subject": subjects,
  "predicateType": "https://slsa.dev/provenance/v1",
  "predicate": {
    "buildDefinition": {
      "buildType": "https://github.com/XDCIndia/xOne/blob/main/ci/release.sh",
      "externalParameters": {"source": e["REPO_URI"], "ref": "refs/tags/" + e["TAG"], "target": "linux-x86_64"},
      "internalParameters": {"RUSTFLAGS": e["RUSTFLAGS"], "SOURCE_DATE_EPOCH": e["SDE"],
                             "command": "cargo auditable build --release --locked -p xone" if e["has_dep"] == "yes" else "cargo build --release --locked -p xone"},
      "resolvedDependencies": [
        {"uri": "git+" + e["REPO_URI"] + "@" + e["TAG"], "digest": {"gitCommit": e["COMMIT"]}},
        {"uri": "Cargo.lock", "digest": {"sha256": e["CARGO_LOCK_SHA"]}},
        {"uri": "rust-toolchain.toml", "digest": {"sha256": e["TOOLCHAIN_SHA"]}},
        {"uri": "ci/release.sh", "digest": {"sha256": e["BUILD_SCRIPT_SHA"]}}]},
    "runDetails": {
      "builder": {"id": e["BUILDER"], "version": {"rustc": e["RUSTC_VV"].splitlines()[0], "cargo": e["CARGO_V"], "cargo-auditable": e["AUDITABLE_V"]}},
      "metadata": {"invocationId": e["COMMIT"][:12] + "-" + e["SDE"], "startedOn": e["STARTED"], "finishedOn": e["FINISHED"]},
      "byproducts": [{"name": "stamp", "content": e["stamp"]}]}},
  "xone:toolchain": e["RUSTC_VV"],
  "xone:stamp": e["stamp"],
  "xone:commit": e["COMMIT"],
  "xone:tag": e["TAG"],
  "xone:embeddedDependencyList": e["has_dep"] == "yes",
  "xone:reproducible": "SOURCE_DATE_EPOCH, path remap and no build id; two builders have not yet compared hashes (see docs/ops/release.md)",
  "xone:warnings": [w for w in e["WARNINGS"].splitlines() if w],
}, indent=2, sort_keys=True))
PY

(cd "$DIST" && sha256sum "$name.tar.gz" "$sbom" "xone-$VERSION.provenance.json" > SHA256SUMS && sha256sum -c SHA256SUMS >/dev/null)
gpg --batch --yes --local-user "$KEY" --detach-sign -o "$DIST/SHA256SUMS.sig" "$DIST/SHA256SUMS"
gpg --batch --verify "$DIST/SHA256SUMS.sig" "$DIST/SHA256SUMS" 2>/dev/null || die "signature does not verify"
echo "release: $stamp"
echo "release: $DIST"; ls -l "$DIST"
