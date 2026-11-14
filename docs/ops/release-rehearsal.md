# Release rehearsal (XB-26, issue #46)

Date 2026-11-12. Host: the build host (Ubuntu 24.04, x86_64, Rust 1.94.0 from `rust-toolchain.toml`).
Branch `feat/xb-26-release` on main `876bb8e`, uncommitted tree overlaid on a throwaway commit in temp clones.
GitHub Actions is on a billing hold, so everything below ran locally. No key of the fleet, no public network,
no fleet host was used. The signing key is a throwaway made inside the test.

## 1. Tools

| Tool | State | Used for |
|---|---|---|
| `cargo-auditable` 0.7.7 | installed with `cargo install --locked` (12 s) | release build; the binary has a `.dep-v0` section |
| `cargo-cyclonedx` 0.5.10 | installed with `cargo install --locked` | SBOM, CycloneDX 1.5 |
| `gpg` 2.4 | present | detached signature (minisign is not installed and was not tried: no offline install path) |
| `shellcheck` | present | `ci/test-packaging.sh` |
| `systemd-analyze` 255 | present | unit verification |
| `docker` 29.3 | present | image build |
| `diffoscope` | not installed, not needed (the hashes were equal) | |
| `objdump`, `readelf`, `strings` | present | section and string comparison |

## 2. Reproducibility

`ci/test-release.sh` makes clone A, then clone B from A (different paths, `/tmp/xrel.XXXX/a` and `/b`),
both at the same tagged commit, and runs `ci/release.sh` in both at the same time with `SOURCE_DATE_EPOCH`
= commit time. Result: **binary and tarball SHA-256 are equal** (full run below). Same user, same
`CARGO_HOME`, same host, so this proves path independence only.

Control, to see which flags matter: the same two clones built with a plain `cargo build --release --locked -p xone`
(no `RUSTFLAGS`) and the same `SOURCE_DATE_EPOCH`:

```
90f30b931ea2fcb96738a739c6151f1380797a14484f61ea38fbaf210ce55895  .../a/target/release/xone
90f30b931ea2fcb96738a739c6151f1380797a14484f61ea38fbaf210ce55895  .../b/target/release/xone
GNU build id b3ea3dfb2de3aea19d989ded3579959950be4423 in both; 0 strings contain the checkout path
```

So no differing sections were found and `diffoscope` was not needed. The release script still sets
`--remap-path-prefix` (checkout, `CARGO_HOME`, sysroot) and `-Wl,--build-id=none`: the control shows this
host's checkout path does not reach the binary, but a different `CARGO_HOME`, rustup sysroot or user would
put other paths into panic locations and std debug strings. That case is **not tested** here (it needs a
second user or machine). Open item for the two-builder gate.

## 3. `ci/test-release.sh` (full transcript of the passing run, gpg chatter removed)

Command: `KEEP=1 ci/test-release.sh` (2 min, 2 builds in parallel).

```
ok   refuses HEAD without a tag
ok   refuses a tag that differs from the workspace version
ok   refuses without XONE_RELEASE_KEY
ok   refuses a dirty tree
ok   release.sh ran in two clones
ok   artifacts present: SHA256SUMS SHA256SUMS.sig xone-0.1.0.cdx.json xone-0.1.0-linux-x86_64.tar.gz xone-0.1.0.provenance.json
ok   SHA256SUMS verifies
ok   signature verifies with the throwaway key and fails on a changed file
ok   xone --version == xOne/v0.1.0-17a160d3-11112026/linux-x86_64/rustc1.94.0
     provenance warnings: [] | embedded dependency list: True
ok   provenance fields filled
ok   SBOM is CycloneDX with components
     binary A bbbe1617b53023e18037c53c89f9c3b663da5d907926bf10edb2c3ebcabc216e
     binary B bbbe1617b53023e18037c53c89f9c3b663da5d907926bf10edb2c3ebcabc216e
     tarball A 155de55016cf290f3b65612ee9ea927a82e460580e0d0e31179f084b31a1efab
     tarball B 155de55016cf290f3b65612ee9ea927a82e460580e0d0e31179f084b31a1efab
ok   two clones in different paths: identical binary and tarball SHA-256
test-release: PASS
```

## 4. Packaging checks: `ci/test-packaging.sh`

```
ok   shellcheck: ops/run-xone.sh ci/release.sh ci/test-release.sh ci/test-packaging.sh
ok   systemd-analyze verify xone@apothem.service: no output, exit 0
ok   unit has the issue contract lines and no read-only datadir option
ok   apothem: profile flag, bootnodes from ops/bootnodes, NODENAME xone-apothem-<host>, jwt.hex created (64 chars, 0600)
ok   jwt.hex kept on the second start
ok   xdcmainnet, xdcdevnet and the xOneGo aliases (mainnet, xdc-devnet)
ok   net2040: chain id 2040, GENESIS required, BOOTNODES from the env
ok   ethstats: default host stats.xdcindia.com:443; no secret default (no --ethstats, warning on stderr)
ok   no secret default in ops/, Dockerfile, Makefile, ci/release.sh
test-packaging: PASS
```

`systemd-analyze verify` runs against a root made of the host's unit files plus the unit and a stub
`/opt/xone`, because the verifier checks that `ExecStart` exists and that its targets resolve. Without that
root it reports `Command /opt/xone/run-xone.sh is not executable`, which is the missing install, not a unit
fault. The unit keeps `User=xone`; `verify` does not check that the user exists.

Unit review against the issue contract: `EnvironmentFile=-/etc/xone/%i.env`, `Restart=on-failure`,
`LimitNOFILE=65536`, `KillSignal=SIGTERM`, `TimeoutStopSec=15` (the node stops in 5 s), `MemoryMax=15G`
present. Added: `Environment=XONE_BIN=/opt/xone/xone` (before the env file, so the env file can override),
`StartLimitIntervalSec=300`/`StartLimitBurst=5`, and hardening (`PrivateDevices`, `ProtectSystem=full`,
`ProtectHome`, `ProtectKernel*`, `ProtectControlGroups`, `RestrictSUIDSGID`, `RestrictRealtime`,
`LockPersonality`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`, `UMask=0077`). `ProtectSystem=strict`
and `ReadWritePaths` were not used: `DATADIR` is the operator's choice, so `full` (read-only `/usr`,
`/boot`, `/etc`) is the setting that cannot break a datadir under `/data`, `/var` or `/opt`. Not run:
`systemctl start/stop` (no systemd privileges here), so the gate "survives `systemctl stop` and `start`"
is shown only by the launcher rehearsal below plus the unit being a normal unit.

## 5. Image

`docker build -f Dockerfile --build-arg VERSION=0.1.0 -t xone:0.1.0 <dist>` built from the tarball of clone A.
`docker run --entrypoint /opt/xone/xone xone:0.1.0 --version` printed
`xOne/v0.1.0-17a160d3-11112026/linux-x86_64/rustc1.94.0` (the stamp of the tarball) and `id` printed
`uid=10001(xone)`. `docker run -e NETWORK=apothem -e XONE_BIN=echo` with `/data` as a tmpfs printed the
launcher line for `--apothem --datadir /data`. Image size 40 MB. A run with the default anonymous volume
failed with `No space left on device`: Docker's root is `/mnt/data`, which is 100% full on this host. That
is the host, not the image. The release binary needs glibc 2.34 at most, so both Ubuntu 24.04 (used) and
Debian 12 run it. `apt-get install ca-certificates` in the image is not pinned (the base is, by digest).
The image build was not repeated for reproducibility.

## 6. Launcher rehearsal, upgrade and rollback on a loopback devnet

`ci/rehearse-release.sh`: geth-XDC from the pin mines 16 blocks of a private chain 5551 (one signer,
loopback, no discovery; `crates/xone-net/tests/local-geth/run.sh`), then serves it. The unpacked releases of
clone A (N, commit `17a160d3`) and of a clone C with one more commit (N+1, `96faabeb`, same tag name and
version) run under `ops/run-xone.sh` from a temp `/etc/xone`-style env file (`NETWORK=xdcdevnet`,
`DATADIR`, `BOOTNODES`, `EXTRA_ARGS` with `--override.genesis`). `set -a; . env` stands in for
`EnvironmentFile=`. Every start checks `web3_clientVersion` equals `xone --version` and the head block hash
equals geth's.

```
[rehearsal] geth: private devnet chain 5551, 16 blocks, loopback (a burst; this geth stops mining after about 17)
prepared: head 17
[rehearsal] geth serves head 17, enode enode://7b63f85997993beafbecd1c0921f46d3934a768f8d4e94155c68512cfd58a661d922e2b7226dac3d720f1b883eb0d608d35e09ebe7dd85a3803508258a8f5f57@127.0.0.1:36689
[rehearsal] 1. release N under the launcher
[xone] run-xone: ETHSTATS_SECRET is not set; starting without --ethstats (node will not show on the stats board)
[xone] run-xone: network=xdcdevnet node=xone-xdcdevnet-ubuntu datadir=/tmp/xreh.R8Y1uJ/data/xdcdevnet
[xone] chain: following from block 0 0x43ddf46515cef135f278120330c189e8f19c449c422d0047e04a37d434825f56
[rehearsal] web3_clientVersion = xOne/v0.1.0-17a160d3-11112026/linux-x86_64/rustc1.94.0 (equals xone --version)
[rehearsal] head block hash 0xfa06e63ed68ad2eb0693cdaaea3712d8499ffb97ca60c3efd246101f62ad8daa equals geth's
[rehearsal] head 17 (geth 17), jwt.hex: 600 64 bytes
[rehearsal] 2. SIGTERM and restart
[rehearsal] SIGTERM: stopped in 0.11s, exit code 0
[rehearsal] restart head 17 (before: 17)
[rehearsal] 3. upgrade N -> N+1 (ops/upgrade-rollback.md section A)
[rehearsal] SIGTERM: stopped in 0.1s, exit code 0
[rehearsal] web3_clientVersion = xOne/v0.1.0-96faabeb-11112026/linux-x86_64/rustc1.94.0 (equals xone --version)
[rehearsal] head block hash 0xfa06e63ed68ad2eb0693cdaaea3712d8499ffb97ca60c3efd246101f62ad8daa equals geth's
[rehearsal] head after upgrade 17 (before: 17)
[rehearsal] 4. rollback N+1 -> N, format unchanged (section B2)
[rehearsal] SIGTERM: stopped in 0.1s, exit code 0
[rehearsal] web3_clientVersion = xOne/v0.1.0-17a160d3-11112026/linux-x86_64/rustc1.94.0 (equals xone --version)
[rehearsal] head block hash 0xfa06e63ed68ad2eb0693cdaaea3712d8499ffb97ca60c3efd246101f62ad8daa equals geth's
[rehearsal] head after rollback 17
[rehearsal] 5. rollback by restoring the pre-upgrade copy (section B3)
[rehearsal] SIGTERM: stopped in 0.1s, exit code 0
[rehearsal] web3_clientVersion = xOne/v0.1.0-17a160d3-11112026/linux-x86_64/rustc1.94.0 (equals xone --version)
[rehearsal] head block hash 0xfa06e63ed68ad2eb0693cdaaea3712d8499ffb97ca60c3efd246101f62ad8daa equals geth's
[rehearsal] head after restore 17 (backup was taken at 17)
[rehearsal] SIGTERM: stopped in 0.1s, exit code 0
[rehearsal] PASS; work dir /tmp/xreh.R8Y1uJ
```

Each restart logged `chain: following from block 17 0xfa06...8daa` (not block 0): the datadir resumed.
Runbook sections A, B2 and B3 of `ops/upgrade-rollback.md` were the steps used. The datadir format did not
change between N and N+1 (there is one format), so the migration branch of the runbook is not exercised;
neither is a signer: the node does not call the safety record yet (`docs/ops/keys.md`), so "a rollback never
lowers the record" is a runbook rule backed by ADR-010 section 5 and its monotonic-record test, not
something this rehearsal measured. Following new blocks after a restart is the live test
(`crates/xone-node/tests/live/run.sh`); this geth stops mining after one burst, so it was not repeated here.
`net2040` (chain 2040): `ops/run-xone.sh` handles the preset, but xone refuses chain id 2040 until
`xone-chainspec` has a net2040 entry (`DatadirError::NoSpec`); the node was not started on net2040.

## 7. Two independent builders (issue #22, 2026-11-14)

Commit `d1d6c3f59c0b56c1c609f7a1f763b85dcf87829d` (clean clone from a `git bundle`, stamp date from the commit),
two hosts, two users' worth of paths: each host has its own `RUSTUP_HOME`, `CARGO_HOME`, checkout and target
directory, all under a different prefix. Same flags as `ci/release.sh`: `SOURCE_DATE_EPOCH` = commit time,
`--remap-path-prefix` for checkout, `CARGO_HOME` and sysroot, `-Wl,--build-id=none`.
Command: `cargo build --release --locked -p xone` (glibc target; the musl/zig path of `ci/release.sh` is not set up on host B).

| Builder | Host | gcc | Build time | SHA-256 of `target/release/xone` |
|---|---|---|---|---|
| A | build host (Ubuntu 24.04.4) | 13.3.0 | 22 min (load above 100) | `2f3f11ef05c416f4a3a50f248a44c66d5b0eed96203a0a8d4f2751d1639d9f38` |
| B | h213 (Ubuntu 24.04.4, other machine) | 13.3.0 | 2 min 42 s | `2f3f11ef05c416f4a3a50f248a44c66d5b0eed96203a0a8d4f2751d1639d9f38` |

Both print `xOneRS/v0.1.0-d1d6c3f5-14112026/linux-x86_64/rustc1.94.0`. The two hashes are equal.
Limits: same OS release and gcc version on both hosts, glibc target only, no signing. A different distro or gcc is not tested.
