# net2040: what each sync mode of xone actually does

Run of 2026-11-12, binary `xOne/v0.1.0-46e87696-12112026/linux-x86_64/rustc1.94.0` (main at 46e8769,
sha256 `088d9d75...0569`), one node per mode, each reporting to `https://skynet.xdcindia.com/old/stats?net=2040`.
Peer: the geth-XDC archive `enode://0337f630...a9@135.181.117.109:35071` (v1.17.10-xdc.21) plus
discovery. Reference: the archive RPC on xdc02 (`127.0.0.1:10071`, read only).

## Summary

| mode | flag | what the build does | board name | host |
| --- | --- | --- | --- | --- |
| full | `--syncmode full` (and no flag) | follows from genesis, executes every block | `xone-rs-net2040-h95` | 95.217.56.168 (see `net2040-follow.md`) |
| snap | `--syncmode snap` | **accepted and ignored**: identical to full, from block 0 | `xone-rs-net2040-snap-h95` | 95.217.56.168 |
| fast | `--syncmode fast` | **accepted and ignored**: identical to full, from block 0 | `xone-rs-net2040-fast-h213` | 65.21.27.213 |
| archive | `--gcmode archive` | **accepted and ignored**: same 128-block state window as full | `xone-rs-net2040-archive-h4` | 65.21.71.4 |
| minimal | `--minimal` after `xone snapshot import` | anchored start, window retention (spec/bootstrap/minimal-mode.md) | `xone-rs-net2040-minimal-h213` | 65.21.27.213 (not yet run) |

Cause (code at 46e8769): `Config::set` checked `syncmode` against `snap|fast|full` and `gcmode` against
`full|archive` and stored the string; nothing reads `cfg.sync.mode`, and `cfg.sync.gcmode` reaches only
the ethstats hello. The RLPx Hello offers `eth` only (`crates/xone-net/src/rlpx.rs`: "With `eth` the only
capability we offer"), and the snap/1 client in `crates/xone-snap` (codec, proofs, state download, healing)
is a dependency of no other crate. The board shows the truth (`syncMode` is hard-coded `"full"` in
`services/mod.rs`), the flag does not.

Fix on `fix/sync-modes`: `snap`, `fast` and `gcmode archive` are refused with a typed error
(`ConfigError::Invalid`, "not implemented in this build ...") from the flag and from the config file
alike, and the default sync mode is `full`. Wiring snap/1 (capability, pivot, state install into the
store, switch to execution) and an archive state store are features, not part of this fix.

## Per mode

### full

Proven in `net2040-follow.md` (MDBX store: genesis to tip in about 95 minutes, 17 GB, peak RSS 7.07 GB,
equality with the archive at 9 heights, SIGTERM restart resumes). The live node was not touched in this run
(it now mints as seat 0xa0ea).

### snap: `xone-rs-net2040-snap-h95`

Host 95.217.56.168, dir `/mnt/data/xone-net2040-snap`, ports p2p 30323, http 8555, ws 8556, metrics 6063.

```
cd /mnt/data/xone-net2040-snap && XONE_ETHSTATS="$E" setsid nohup ./run.sh >/dev/null 2>&1 &
# run.sh: xone --network net2040 --datadir ./data --bootnodes <archive enode> --port 30323
#   --http --http.addr 0.0.0.0 --http.port 8555 --http.api eth,net,web3,XDPoS --ws ... --ws.port 8556
#   --identity xone-rs-net2040-snap-h95 --metrics --metrics.addr 127.0.0.1 --metrics.port 6063
#   --syncmode snap --gcmode full [--ethstats "$XONE_ETHSTATS"]
```

Log (start 03:05:29 UTC):

```
started chain 2040 genesis 0x8d8ff2e9d8369b7b4af5a84eb010e2b1a358eefa9f971985435d538dda235714
chain: following from block 0 0x8d8ff2e9...35714
chain: peer 0337f630 connected, eth/101, rank 821510
chain: imported 128 blocks, head 128
chain: imported 128 blocks, head 256
```

No pivot, no snap/1: 72 peer sessions in the log, all `eth/101`; `grep -c 'snap/1\|pivot'` is 0; the
first executed block is 1. The board entry: `active:true`, clientVersion as above, `syncMode:"full"`.
State below the window is refused like on any full node:

```
eth_getBalance 0x…88 @0x10 -> -32000 unsupported history: State of block 16 is below the retained history (first retained 3518, pivot 0)
```

Stopped by SIGTERM at block 21,981 (03:18 UTC, exit 0 in 1 s, state checkpoint written) because free space on
`/mnt/data` fell from 41 GB to 21 GB in 13 minutes, mostly from writers outside this run, with the live
minting node on the same disk. Measured until then: 21,981 blocks in 12.5 min with the host at load 60,
datadir 2.2 GB, peak RSS 1.82 GB. Not run to the tip; nothing more would be learned about snap from it:
it is the full sync.

### fast: `xone-rs-net2040-fast-h213`

Host 65.21.27.213, dir `/data/xone-net2040-fast`, ports p2p 30624, http 8657 (127.0.0.1), ws 8658, metrics
6164, `--nat extip:65.21.27.213 --syncmode fast`. Started 03:19:18 UTC (loop pid 1513253).

```
start xOne/v0.1.0-46e87696-... --network net2040 ... --syncmode fast --ethstats <set>
chain: following from block 0 0x8d8ff2e9...35714
chain: peer 0337f630 connected, eth/101, rank 821821
```

Same as snap: `fast` is in the accepted list of `config.rs` and read by nothing (`bin/xone/src/flags.rs`
only documents it). No `snap/1` or `pivot` line. Board: `active:true`, `syncMode:"full"`.

### archive: `xone-rs-net2040-archive-h4`

Host 65.21.71.4, dir `/mnt/data/xone-net2040-archive`, ports p2p 30626, http 8661 (127.0.0.1), ws 8662,
metrics 6166, `--nat extip:65.21.71.4 --syncmode full --gcmode archive`. Board: `active:true`,
`gcMode:"archive"`. Old state, while the node was at about block 22,000:

```
archive-h4  eth_getBalance  0x…88 @0x10 -> -32000 unsupported history: State of block 16 is below the retained history (first retained 21143, pivot 0)
archive-h4  eth_getStorageAt 0x…88 0x0 @0x10 -> same error
snap-h95 (--gcmode full) eth_getBalance 0x…88 @0x10 -> same error (first retained 3518)
geth archive eth_getBalance 0x…88 @0x10 -> 0x3f870857a3e0e3800000
```

The flag does not change what the node keeps: `--gcmode archive` is a full node with a mislabel on the board.

### minimal: `xone-rs-net2040-minimal-h213`

Host 65.21.27.213, dir `/data/xone-net2040-minimal`, ports p2p 30625, http 8659 (127.0.0.1), ws 8660,
metrics 6165, `--minimal`. Not run yet: it needs a datadir at the tip to export from. The source is the
fast node on the same host (a full datadir, as shown above). Steps, on 65.21.27.213 once
`xone-rs-net2040-fast-h213` is within 3 blocks of the archive:

```
# 1. stop the fast loop and node (the export opens the datadir like start does)
touch /data/xone-net2040-fast/STOP; kill -TERM <xone pid whose ppid is $(cat /data/xone-net2040-fast/run.pid)>
# 2. export (prints "exported block N <hash> root ...")
/data/xone-net2040-fast/xone snapshot export --network net2040 --datadir /data/xone-net2040-fast/data --out /data/xone-net2040-minimal/net2040.xsnap
# 3. restart the fast loop
rm /data/xone-net2040-fast/STOP; cd /data/xone-net2040-fast && env XONE_ETHSTATS=<fast value> setsid nohup ./run.sh >/dev/null 2>&1 </dev/null &
# 4. pins: block N from step 2, hash from the archive (from the operator host: ssh xdc02 curl eth_getBlockByNumber N)
echo "N 0x<archive hash of N>" > /data/xone-net2040-minimal/pins.txt
# 5. import, then run
/data/xone-net2040-minimal/xone snapshot import --network net2040 --datadir /data/xone-net2040-minimal/data --in /data/xone-net2040-minimal/net2040.xsnap --pins /data/xone-net2040-minimal/pins.txt
cd /data/xone-net2040-minimal && env XONE_ETHSTATS=<minimal value> setsid nohup ./run.sh >/dev/null 2>&1 </dev/null &
```

## Other modes and subcommands of the CLI (`docs/cli/flags.md`)

| surface | tested here |
| --- | --- |
| `--syncmode full` | yes, through the live node (`net2040-follow.md`) |
| `--syncmode snap`, `--syncmode fast` | yes: accepted and ignored at 46e8769, refused on `fix/sync-modes` |
| `--gcmode full` | yes (snap-h95, fast-h213) |
| `--gcmode archive` | yes: accepted and ignored at 46e8769, refused on `fix/sync-modes` |
| `--minimal`, `snapshot export`, `snapshot import` | prepared, not run (above) |
| `--syncfromblock`, `import <file>`, `export <file>` | not tested in this run |

## What is not covered

* snap/1 sync. The pieces exist (`crates/xone-snap`) but no node path uses them: a snap/1 capability in
  `xone-net`, pivot choice, a `PeerSource` over the session, installing the downloaded state as the store's
  anchor and the handover to block execution. The client and the server are exercised only by
  `conformance/snap` (`tests/live.rs`: two loopback instances against a private geth-XDC, with their own
  Hello). The same holds at b4e2ffe, the build of the live node: it offers `eth` only, so it cannot be the
  snap/1 server of another node either.
* an archive state store (historical state served for every block).
