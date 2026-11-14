#!/usr/bin/env bash
# OS packages (issue #228). Called by ci/release.sh after the tarballs exist; also runnable alone:
#
#   XONE_DIST=dist [XONE_RELEASE_KEY=<gpg key id>] ci/package.sh
#
# For each xone-<version>-linux-<arch>-musl.tar.gz in XONE_DIST: xone_<version>_<deb arch>.deb (dpkg-deb)
# and xone-<version>.<rpm arch>.rpm (nfpm, ops/packaging/nfpm.yaml; skipped with a warning if nfpm is
# missing). The binary comes out of the tarball, so it is byte-equal to the tarball's. Then the
# repository layout under XONE_DIST/repo/: apt/ (pool, dists/stable, signed Release and InRelease when a
# key is set) and yum/ (the rpms; `createrepo_c` metadata when installed). Package scripts never
# start, stop or restart a unit: the only script is ops/packaging/postinst.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
DIST=$(cd "${XONE_DIST:-$root/dist}" && pwd)
SDE=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
KEY="${XONE_RELEASE_KEY:-}"
made=()
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
warn() { echo "package: WARNING: $*" >&2; }

for tgz in "$DIST"/xone-*-linux-*-musl.tar.gz; do
  [ -e "$tgz" ] || { echo "package: no tarball in $DIST" >&2; exit 1; }
  base=$(basename "$tgz" .tar.gz)               # xone-<version>-linux-<arch>-musl
  arch=${base#*-linux-}; arch=${arch%-musl}
  version=${base#xone-}; version=${version%-linux-*}
  case $arch in x86_64) deb=amd64 nf=amd64 ;; aarch64) deb=arm64 nf=arm64 ;; *) echo "package: unknown arch $arch" >&2; exit 1 ;; esac

  s=$tmp/$arch/stage; mkdir -p "$s"
  tar -xzf "$tgz" -C "$tmp/$arch"
  src=$tmp/$arch/$base
  install -Dm755 "$src/xone" "$s/usr/bin/xone"
  install -Dm755 "$src/run-xone.sh" "$s/usr/lib/xone/run-xone.sh"
  # the packaged unit differs from the tarball's in paths only
  mkdir -p "$s/usr/lib/systemd/system"
  sed -e 's#/opt/xone/xone#/usr/bin/xone#' -e 's#/opt/xone/run-xone.sh#/usr/lib/xone/run-xone.sh#' \
      -e 's#^Environment=XONE_BIN=.*#&\nEnvironment=XONE_BOOTNODES_DIR=/usr/share/xone/bootnodes#' \
      "$src/xone@.service" > "$s/usr/lib/systemd/system/xone@.service"
  chmod 644 "$s/usr/lib/systemd/system/xone@.service"
  mkdir -p "$s/usr/share/xone" "$s/usr/share/doc/xone"
  cp -r "$src/bootnodes" "$s/usr/share/xone/bootnodes"
  install -m644 ops/packaging/apothem.env.example "$s/usr/share/xone/apothem.env.example"
  install -m644 "$src/LICENSE" "$s/usr/share/doc/xone/LICENSE"
  find "$s" -exec touch -h -d "@$SDE" {} +

  # .deb
  mkdir -p "$s/DEBIAN"
  cat > "$s/DEBIAN/control" <<CTL
Package: xone
Version: $version
Architecture: $deb
Maintainer: AnilChinchawale <29918376+AnilChinchawale@users.noreply.github.com>
Section: net
Priority: optional
Installed-Size: $(du -sk "$s/usr" | cut -f1)
Homepage: https://github.com/XDCIndia/xOne
Description: xOneRS, a Rust XDC Network client
 Static binary, the xone@.service template, the bootnode lists and the
 launcher. Installing never starts a node: enable xone@<network> yourself.
CTL
  install -m755 ops/packaging/postinst "$s/DEBIAN/postinst"
  touch -h -d "@$SDE" "$s/DEBIAN" "$s/DEBIAN/control" "$s/DEBIAN/postinst"
  out=$DIST/xone_${version}_$deb.deb
  dpkg-deb --root-owner-group -Zxz -b "$s" "$out" >/dev/null
  rm -rf "$s/DEBIAN"
  made+=("$(basename "$out")")

  # .rpm
  if command -v nfpm >/dev/null 2>&1; then
    rpm=$DIST/xone-$version.$arch.rpm
    ARCH=$nf VERSION=$version STAGE=$s nfpm package -f ops/packaging/nfpm.yaml -p rpm -t "$rpm" >/dev/null
    made+=("$(basename "$rpm")")
  else
    warn "nfpm is not installed: no .rpm for $arch (https://nfpm.goreleaser.com)"
  fi
done

# Repository layout
repo=$DIST/repo; rm -rf "$repo"; mkdir -p "$repo/apt/pool/main" "$repo/yum"
cp "$DIST"/*.deb "$repo/apt/pool/main/"
for a in amd64 arm64; do
  d=$repo/apt/dists/stable/main/binary-$a; mkdir -p "$d"
  (cd "$repo/apt" && dpkg-scanpackages --arch "$a" pool/main 2>/dev/null > "dists/stable/main/binary-$a/Packages")
  gzip -9n -k "$d/Packages"
done
(cd "$repo/apt" && apt-ftparchive -o APT::FTPArchive::Release::Codename=stable -o APT::FTPArchive::Release::Suite=stable \
   -o APT::FTPArchive::Release::Architectures="amd64 arm64" -o APT::FTPArchive::Release::Components=main release dists/stable > dists/stable/Release)
if [ -n "$KEY" ]; then
  gpg --batch --yes --local-user "$KEY" --detach-sign --armor -o "$repo/apt/dists/stable/Release.gpg" "$repo/apt/dists/stable/Release"
  gpg --batch --yes --local-user "$KEY" --clearsign -o "$repo/apt/dists/stable/InRelease" "$repo/apt/dists/stable/Release"
  gpg --batch --yes --export --armor "$KEY" > "$repo/xone-release.pub"
else
  warn "XONE_RELEASE_KEY is not set: the apt Release file is not signed"
fi
if ls "$DIST"/*.rpm >/dev/null 2>&1; then
  cp "$DIST"/*.rpm "$repo/yum/"
  if command -v createrepo_c >/dev/null 2>&1; then
    createrepo_c -q "$repo/yum"
    [ -z "$KEY" ] || gpg --batch --yes --local-user "$KEY" --detach-sign --armor -o "$repo/yum/repodata/repomd.xml.asc" "$repo/yum/repodata/repomd.xml"
  else
    warn "createrepo_c is not installed: yum/ holds the rpms without metadata"
  fi
fi
printf 'package: %s\n' "${made[@]}"
