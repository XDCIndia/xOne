#!/usr/bin/env bash
# Packaging checks (XB-26): shellcheck, `systemd-analyze verify` on the unit template, the unit's
# contract lines, and the launcher's NETWORK presets (the launcher runs `echo` instead of xone).
set -euo pipefail
cd "$(dirname "$0")/.."
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok   $*"; }

shellcheck ops/run-xone.sh ci/release.sh ci/test-release.sh ci/test-packaging.sh && ok "shellcheck: ops/run-xone.sh ci/release.sh ci/test-release.sh ci/test-packaging.sh"

R=$(mktemp -d); trap 'rm -rf "$R"' EXIT
mkdir -p "$R/opt/xone" "$R/etc/systemd/system" "$R/usr/lib/systemd"
cp -r /usr/lib/systemd/system "$R/usr/lib/systemd/"          # the host's targets, so the unit's dependencies resolve
install -m755 ops/run-xone.sh "$R/opt/xone/run-xone.sh"; install -m755 /bin/true "$R/opt/xone/xone"
cp ops/xone@.service "$R/etc/systemd/system/"
out=$(systemd-analyze verify --root="$R" /etc/systemd/system/xone@apothem.service 2>&1) || fail "systemd-analyze verify: $out"
[ -z "$out" ] || fail "systemd-analyze verify printed: $out"
ok "systemd-analyze verify xone@apothem.service: no output, exit 0"

for l in 'EnvironmentFile=-/etc/xone/%i.env' 'Restart=on-failure' 'LimitNOFILE=65536' 'KillSignal=SIGTERM' 'TimeoutStopSec=15' 'NoNewPrivileges=true' 'ProtectSystem=full'; do
  grep -qxF "$l" ops/xone@.service || fail "unit lacks $l"
done
! grep -q '^ProtectSystem=strict\|^ReadOnlyPaths\|^ProtectHome=read-only' ops/xone@.service || fail "unit would make the datadir read-only"
ok "unit has the issue contract lines and no read-only datadir option"

launch() { env -i PATH="$PATH" HOME=/nonexistent XONE_BIN=echo DATADIR="$D/d" "$@" ops/run-xone.sh 2>"$D/err"; }
D=$(mktemp -d); trap 'rm -rf "$R" "$D"' EXIT
chk() { case "$out" in *"$1"*) ;; *) fail "$2: expected '$1' in: $out" ;; esac; }
out=$(launch NETWORK=apothem); chk "--apothem" apothem; chk "--bootnodes enode://" "apothem bootnodes"; chk "--identity xone-apothem-$(hostname -s)" nodename; chk "--authrpc.jwtsecret $D/d/jwt.hex" jwt
[ "$(wc -c < "$D/d/jwt.hex")" = 64 ] || fail "jwt.hex is not 64 chars"
[ "$(stat -c %a "$D/d/jwt.hex")" = 600 ] || fail "jwt.hex mode is not 600"
ok "apothem: profile flag, bootnodes from ops/bootnodes, NODENAME xone-apothem-<host>, jwt.hex created (64 chars, 0600)"
jwt=$(cat "$D/d/jwt.hex"); launch NETWORK=apothem >/dev/null; [ "$jwt" = "$(cat "$D/d/jwt.hex")" ] || fail "jwt.hex replaced on second start"
ok "jwt.hex kept on the second start"
out=$(launch NETWORK=xdcmainnet); chk "--xdcmainnet" mainnet; chk "xone-xdcmainnet-" nodename
out=$(launch NETWORK=mainnet); chk "--xdcmainnet" "alias mainnet"
out=$(launch NETWORK=xdc-devnet); chk "--xdcdevnet" "alias xdc-devnet"
out=$(launch NETWORK=xdcdevnet); chk "--xdcdevnet" devnet
ok "xdcmainnet, xdcdevnet and the xOneGo aliases (mainnet, xdc-devnet)"
out=$(launch NETWORK=net2040 GENESIS=/g.json BOOTNODES=enode://aa@1.2.3.4:1); chk "--override.genesis /g.json --networkid 2040" net2040; chk "--bootnodes enode://aa@1.2.3.4:1" "net2040 bootnodes"
! launch NETWORK=net2040 >/dev/null || fail "net2040 without GENESIS did not stop"
ok "net2040: chain id 2040, GENESIS required, BOOTNODES from the env"
out=$(launch NETWORK=apothem ETHSTATS_SECRET=s3cret); chk "--ethstats xone-apothem-$(hostname -s):s3cret@stats.xdcindia.com:443" ethstats
out=$(launch NETWORK=apothem); case "$out" in *--ethstats*) fail "ethstats without a secret" ;; esac; grep -q "ETHSTATS_SECRET is not set" "$D/err" || fail "no warning"
ok "ethstats: default host stats.xdcindia.com:443; no secret default (no --ethstats, warning on stderr)"
! grep -rIE '(^|[^A-Za-z_$])ETHSTATS_SECRET=\"?[A-Za-z0-9]{8,}|BEGIN (PGP|RSA|EC|OPENSSH) PRIVATE' ops Dockerfile Makefile ci/release.sh >/dev/null || fail "secret-like text in ops"
ok "no secret default in ops/, Dockerfile, Makefile, ci/release.sh"
echo "test-packaging: PASS"
