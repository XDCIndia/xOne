# net2040: what each sync mode of xone actually does

Run of 2026-11-12, binary `xOne/v0.1.0-46e87696-12112026/linux-x86_64/rustc1.94.0` (main at 46e8769,
sha256 `088d9d75...0569`), one node per mode, each reporting to `https://skynet.xdcindia.com/old/stats?net=2040`.
Peer: the geth-XDC archive `enode://0337f630...a9@135.181.117.109:35071` (v1.17.10-xdc.21) plus
discovery. Reference: the archive RPC on xdc02 (`127.0.0.1:10071`, read only).

## Summary

| mode | flag | what the build does | board name | host |
| --- | --- | --- | --- | --- |
| full | `--syncmode full` (and no flag) | follows from genesis, executes every block | `xone-rs-net2040-h95` | 95.217.56.168 (see `net2040-follow.md`) |
| snap | `--syncmode snap` | state at a pivot over snap/1 + xpre/1 from an xOne peer, installed as an anchored datadir, executes from pivot + 1 (second run below; first run: accepted and ignored) | `xone-rs-net2040-snap-h95` | 95.217.56.168 |
| fast | `--syncmode fast` | alias of snap, as in geth-XDC (second run below) | `xone-rs-net2040-fast-h213` | 65.21.27.213 |
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

Feature on `feat/xb-17-snap-in-node` (XB-17): every node offers and serves snap/1 and xpre/1 (xOne's preimage
capability), `--syncmode snap` fills an empty datadir at a pivot and `fast` is its alias; `gcmode archive`
stays refused. Rules: `spec/snap/pivot.md`, `spec/snap/preimages.md`, `spec/bootstrap/snap-install.md`. The
snap and fast sections below are the second run, on that build.

## Per mode

### full

Proven in `net2040-follow.md` (MDBX store: genesis to tip in about 95 minutes, 17 GB, peak RSS 7.07 GB,
equality with the archive at 9 heights, SIGTERM restart resumes). The live node was not touched in this run
(it now mints as seat 0xa0ea).

### snap: `xone-rs-net2040-snap-h95`

Second run, 2026-11-12 06:01 UTC, build `xOne/v0.1.0-24e39236-12112026` (branch `feat/xb-17-snap-in-node`,
main 7cab738 merged; sha256 `4eb17d33...1f4f`). Host 95.217.56.168, dir `/mnt/data/xone-net2040-snap`, fresh
datadir, ports p2p 30323, http 8555, ws 8556, metrics 6063.

```
# run.sh: xone --network net2040 --datadir ./data --bootnodes <h4 enode>,<geth archive enode> --port 30323
#   --http ... --identity xone-rs-net2040-snap-h95 --metrics ... --syncmode snap --gcmode full [--ethstats "$XONE_ETHSTATS"]
```

The snap source is `xone-rs-net2040-archive-h4` (65.21.71.4, enode `462cba59…c996@65.21.71.4:30626`) on the
same build, following in full; it serves snap/1 and xpre/1 from its retained state window. The geth archive
shares snap/1 only (no xpre/1), so it is a header witness, not a state source. Log, start 06:01:12Z:

```
snap: --syncmode snap on an empty datadir: state download at a pivot (snap/1, xpre/1)
snap: peer 0337f630 connected, eth/101, shared snap/1
snap: peer 462cba59 connected, eth/101, shared snap/1 xpre/1
snap: head 772333 0x546ce225…1286 from peer 462cba59; headers 767706..=772333 downloaded and linked (4628 headers)
snap: pivot block 772269 0xe3b4a9a9…a57f state root 0x0339032f…f333 (head 772333 - 64); anchor verified: switch 771654
      0x12fa8cee…7923 with 13 masternodes certifies it by the 3-chain to 772272; pin: peers 0337f630,462cba59 serve switch 771654 0x12fa8cee…7923
snap: window: headers 767770..=772269, bodies and receipts 770471..=772269 (1799 blocks, 248 with sign transactions), each checked against its header
snap: gap block 769950 root 0x5f0530dc…ce82: 0x88 account and 170 slots proven, 14 candidates
snap: gap block 770850 root 0xc27933f6…79b7: 0x88 account and 170 slots proven, 14 candidates
snap: gap block 771750 root 0x677551c5…9e75: 0x88 account and 170 slots proven, 14 candidates
snap: state download at root 0x0339032f…f333 from peer 462cba59
snap: state downloaded in 0 s: 9532 accounts, 10920 slots, 6 codes; requests: 4 account ranges, 5 storage ranges, 1 bytecodes, 0 trie nodes (heal); every range proven
snap: heal: 0 trie-node requests; state root 0x0339032f…f333 verified at the pivot 772269
snap: 20430 preimages over xpre/1 in 20 requests, each checked by its hash; state root 0x0339032f…f333 rebuilt from the plain rows
snap: installed pivot 772269 as an anchored datadir in 2 s total (2 MB over snap/1 and xpre/1); executing from 772270
started chain 2040 genesis 0x8d8ff2e9…35714
chain: following from block 772269 0xe3b4a9a9…a57f
chain: imported 65 blocks, head 772334
```

The serving side, on h4:

```
snap: serving root 0x5f0530dc…ce82 of block 769950: 9532 accounts, 10914 slots, built in 319 ms
snap: serving root 0xc27933f6…79b7 of block 770850: 9532 accounts, 10917 slots, built in 161 ms
snap: serving root 0x677551c5…9e75 of block 771750: 9532 accounts, 10920 slots, built in 131 ms
snap: serving root 0x0339032f…f333 of block 772269: 9532 accounts, 10920 slots, built in 42 ms
```

Heal is 0 because the pivot stays fixed and the source serves exactly that root; healing runs only when the
download does not hash to the root. The first executed block is 772,270 = pivot + 1 (the first batch is
772,270..772,334). `<datadir>/xone/snap-pivot.txt` records the pivot, its root, the switch and the pin source.

| measure | snap-h95 | full node (`net2040-follow.md`) |
| --- | --- | --- |
| time from start to the archive head | about 20 s (2 s download and install, then 65 blocks) | about 95 min |
| datadir | 72 MB | 17 GB (20 GB with segments) |
| RSS after the install | 78 MB | peak 7.07 GB |
| data over snap/1 and xpre/1 | about 2 MB | none |

Equality with the archive (xdc02 `127.0.0.1:10071`, `eth_getBlockByNumber`, whole object): equal at 772,269
(pivot), 772,270 (pivot + 1), 772,290, 772,310, 772,330 and 772,343 (head - 10), and at 772,300 with full
transactions; `XDPoS_getMasternodesByNumber` equal at the head 772,357 (epoch 862, 13 masternodes, 1 penalty).

SIGTERM by exact pid at 06:02:52Z: `chain: state checkpoint of block 772378 written`, `stopped`, exit 0; the loop
restarted it at 06:02:57Z: `snap: the datadir holds a chain: continuing it (snap sync fills an empty datadir only)`,
`chain: following from block 772378`; equal with the archive again at 772,380, at head - 10 and for the
masternodes at the head. Board: `xone-rs-net2040-snap-h95` active, `syncMode:"snap"`, clientVersion
`24e39236`.

Below the window the node refuses with the typed error, as an imported datadir does:

```
eth_getBlockByNumber 767000 -> unsupported history: Body of block 767000 is below the retained history (first retained 770471, ...)
eth_getBalance 0x…88 @772169 -> unsupported history: State of block 772169 is below the retained history (first retained 768286, pivot 0)
```

The `firstRetained` of the State refusal is `head - 4096` even where the anchored state starts at the pivot
(`services/rpc.rs` `StateApi::state`); the refusal is right, its floor field is low. Same on an imported datadir.

Refusal without an xOne peer (the geth archive alone, `--syncmode fast`, scratch datadir): `snap: peer 0337f630
connected, eth/101, shared snap/1`, then after 120 s `xone: snap sync: no peer serves xpre/1 (preimages; only xOne
nodes do) with snap/1 after 120s: snap sync needs one, refusing (not falling back to full sync)`, exit 2, the
datadir holds only its key and chain marker.

Timing constraint: a source serves only the roots it holds changesets for. The deepest root a client asks for is
the gap block up to `3E + G - 1` = 3,149 below its pivot, and the pivot is 64 below the head, so the source needs
about 3,213 blocks of state: the window is 4,096 blocks since this branch (ADR-005 section 16). A node upgraded
from a build with 1,024 starts with 1,024 and grows by one per block; h4 was swapped at 04:51Z and could serve the
clients from about 06:00Z (window measured by bisection of `eth_getBalance`: 2,776 blocks at head 772,304).

### fast: `xone-rs-net2040-fast-h213`

Second run, 2026-11-12 06:00:48Z, same build. Host 65.21.27.213, dir `/data/xone-net2040-fast`, fresh datadir,
ports p2p 30624, http 8657 (127.0.0.1), ws 8658, metrics 6164, `--nat extip:65.21.27.213 --syncmode fast`,
bootnodes h4 and the geth archive.

```
--syncmode fast is an alias of snap, as in geth-XDC: syncing in snap mode
snap: --syncmode snap on an empty datadir: state download at a pivot (snap/1, xpre/1)
snap: peer 0337f630 connected, eth/101, shared snap/1
snap: peer 462cba59 connected, eth/101, shared snap/1 xpre/1
snap: head 772321 0x8f85795f…7b51 from peer 462cba59; headers 767694..=772321 downloaded and linked (4628 headers)
snap: pivot block 772257 0x0578b690…2522 state root 0xfc72409d…54c1 (head 772321 - 64); anchor verified: switch 771654
      0x12fa8cee…7923 with 13 masternodes certifies it by the 3-chain to 772260; pin: peers 0337f630,462cba59 serve switch 771654 …
snap: window: headers 767758..=772257, bodies and receipts 770459..=772257 (1799 blocks, 249 with sign transactions), each checked against its header
snap: gap block 769950 …, 770850 …, 771750 …: 0x88 account and 170 slots proven, 14 candidates (each)
snap: state downloaded in 0 s: 9532 accounts, 10920 slots, 6 codes; requests: 4 account ranges, 5 storage ranges, 1 bytecodes, 0 trie nodes (heal); every range proven
snap: heal: 0 trie-node requests; state root 0xfc72409d…54c1 verified at the pivot 772257
snap: 20430 preimages over xpre/1 in 20 requests, each checked by its hash; state root 0xfc72409d…54c1 rebuilt from the plain rows
snap: installed pivot 772257 as an anchored datadir in 2 s total (2 MB over snap/1 and xpre/1); executing from 772258
chain: following from block 772257 0x0578b690…2522
chain: imported 65 blocks, head 772322
```

Datadir 72 MB, RSS 79 MB. Equal with the archive at 772,257 (pivot), 772,258 (pivot + 1), 772,280, 772,300,
772,330 and 772,355 (head - 10), and `XDPoS_getMasternodesByNumber` at the head 772,365. Board:
`xone-rs-net2040-fast-h213` active, `syncMode:"snap"`.

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

Since 04:51Z on the XB-17 build the node runs without `--gcmode archive` (refused) and is the snap/1 and xpre/1
source of the snap and fast runs above.

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
| `--syncmode snap`, `--syncmode fast` | yes: accepted and ignored at 46e8769, refused on `fix/sync-modes`, snap sync on `feat/xb-17-snap-in-node` (above) |
| `--snap.pins` | not run here (the two-peer pin was used); the pins file form is the one of `snapshot import` |
| `--gcmode full` | yes (snap-h95, fast-h213) |
| `--gcmode archive` | yes: accepted and ignored at 46e8769, refused on `fix/sync-modes` |
| `--minimal`, `snapshot export`, `snapshot import` | prepared, not run (above) |
| `--syncfromblock`, `import <file>`, `export <file>` | not tested in this run |

## What is not covered

* snap sync from geth-only peers: refused by design (no preimages in snap/1, `spec/snap/preimages.md`); the
  hashed-key execution path that would remove this is a TODO row in `spec/snap/pivot.md`.
* a moving pivot and a resumed download: the net2040 state (9,532 accounts) downloads in under a second, so
  neither was exercised; a stop before the install leaves the datadir empty and the next start begins again.
* an archive state store (historical state served for every block).
