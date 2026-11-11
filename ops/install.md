# Install a follower on a clean VM

Target: Ubuntu 24.04 (the release is built there), x86_64, root access, outbound TCP 30303 and UDP 30303,
50 GB free in `/data`. A follower only: no key, no keystore, no `--mine`. Plan: 15 minutes or less.
Time each step and write it in the table at the end. The 15-minute dry run by someone who did not write
this page is still to do (issue #46).

Set the version once: `V=0.1.0` (the tag without `v`).

| # | Step | Command | Target |
|---|---|---|---|
| 1 | Get the artifacts | `curl -fLO` the four files of the release: `xone-$V-linux-x86_64.tar.gz`, `xone-$V.provenance.json`, `xone-$V.cdx.json`, `SHA256SUMS`, and `SHA256SUMS.sig` | 2 min |
| 2 | Import the release public key you were given by the owner (key id is in the release notes) | `gpg --import xone-release.pub` | 1 min |
| 3 | Verify | `gpg --verify SHA256SUMS.sig SHA256SUMS && sha256sum -c SHA256SUMS` — both must say good; stop on anything else | 1 min |
| 4 | Install | `tar -xzf xone-$V-linux-x86_64.tar.gz && install -d /opt/xone && cp -r xone-$V-linux-x86_64/. /opt/xone/` | 1 min |
| 5 | Check the stamp | `/opt/xone/xone --version` must read `xOne/v$V-<commit8>-<DDMMYYYY>/linux-x86_64/rustc1.94.0`, with no `unknown` and no `-dirty` | 30 s |
| 6 | User and data | `useradd --system --home-dir /data/xone --shell /usr/sbin/nologin xone && install -d -o xone -g xone -m 0700 /data/xone` | 1 min |
| 7 | Unit | `install -m644 /opt/xone/xone@.service /etc/systemd/system/ && systemctl daemon-reload` | 30 s |
| 8 | Env file | `install -d -m750 /etc/xone && install -m600 /dev/null /etc/xone/apothem.env`, then put `DATADIR=/data/xone/apothem` and `ETHSTATS_SECRET=<from the fleet secret store>` in it | 2 min |
| 9 | Start | `systemctl enable --now xone@apothem` | 30 s |
| 10 | Prove | `journalctl -u xone@apothem -n 50` shows `following from` and `connected`; `curl -s -X POST -H 'content-type: application/json' --data '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' 127.0.0.1:8545` returns the same stamp as step 5; `eth_blockNumber` rises | 3 min |

`NETWORK` is the instance name (`xone@<net>`): `apothem`, `xdcmainnet`, `xdcdevnet` or `net2040`.
Bootnodes come from `/opt/xone/bootnodes/<net>.txt`; set `BOOTNODES=enode://...,enode://...` in the env
file to replace them. `net2040` (chain 2040) needs `GENESIS=/path/net2040-genesis.json` and a `BOOTNODES`
line, and xone refuses chain id 2040 until its chain spec has a net2040 entry (`NoSpec(2040)`; see
`docs/ops/release-rehearsal.md`). Without `ETHSTATS_SECRET` the node runs but is invisible on
stats.xdcindia.com. The RPC listens on loopback only unless the env file says otherwise
(`EXTRA_ARGS=--http.addr ...`); do not open 8545 or 8551 to the internet.

Existing `/etc/xone/*.env` files from xOneGo hosts work unchanged: the variable names are the same.

## Stop, start, remove

`systemctl stop xone@apothem` is a bounded stop (SIGTERM, 5 s in the node, SIGKILL after 15 s). `start`
resumes at the same head. The unit is a normal unit, so `stop` does not delete it (the xOneGo
transient-unit problem). `systemctl disable --now xone@apothem` and `rm -r /data/xone/apothem` remove a node.

## Docker

`make release XONE_RELEASE_KEY=...` then `make docker` makes `xone:$V` from the verified tarball. Run:
`docker run -d --name xone-apothem -e NETWORK=apothem --env-file /etc/xone/apothem.env -v xone-apothem:/data -p 30303:30303 -p 30303:30303/udp xone:$V`.
The image runs as uid 10001 and has `/data` as its datadir.

## Timing record

| Step | Minutes | Who | Date |
|---|---|---|---|
| (not yet timed by a second person) | | | |
