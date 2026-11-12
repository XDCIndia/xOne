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

## Resync 2026-11-12, second run

All three anchored nodes were wiped (`rm -rf data`) and resynced one at a time, build 24e39236. The
full producer `xone-rs-net2040-h95` (1676b87) and `xone-rs-net2040-archive-h4` kept running. Time is
from the start of the loop (after the old loop exited) to "following at the tip": node head within 1
of archive-h4 for three polls in a row. Peak RSS is VmRSS sampled every second.

| Node | Flag | Time to tip | Data moved | Datadir at tip | Peak RSS | Pivot / anchor |
| --- | --- | --- | --- | --- | --- | --- |
| `xone-rs-net2040-snap-h95` | `--syncmode snap` | 12 s (install 4 s) | 2 MB over snap/1 + xpre/1 | 72 MB | 80,668 kB | pivot 776638 (head 776702 - 64) |
| `xone-rs-net2040-fast-h213` | `--syncmode fast` | 12 s (install 3 s) | 2 MB over snap/1 + xpre/1 | 72 MB | 79,604 kB | pivot 776694 (head 776758 - 64) |
| `xone-rs-net2040-minimal-h213` | `--minimal` after import | 13 s (import 1.0 s) | 5,187,407 B snapshot | 72 MB | 55,920 kB follow, 46,312 kB import | anchor 776785 |

Mechanism evidence (node.log):

- snap: `shared snap/1 xpre/1` with 462cba59 (archive-h4); headers 772075..=776702 linked; window
  bodies and receipts 774840..=776638; 4 gap blocks with 0x88 proven; 9532 accounts, 10935 slots,
  6 codes in 5 account ranges, 6 storage ranges, 1 bytecodes, 0 heal requests; `state root 0xb2cf721a..
  verified at the pivot 776638`; 20445 preimages over xpre/1; `executing from 776639`.
- fast: first line `--syncmode fast is an alias of snap, as in geth-XDC: syncing in snap mode`, then the
  same steps; root 0x27e9597c.. verified at 776694; `executing from 776695`.
- minimal: `xone snapshot export` on archive-h4 (loop stopped, export, loop restarted with its env):
  `exported block 776785 0x849eddb3..042483 ... 5187407 bytes in 2.8s`, sha256 e9f7a6d5..63107249.
  pins.txt `776785 0x849eddb3ae705f4be624afc4dc404f8ccfa82327ea3d2394487178b32d042483` (archive-h4 hash,
  equal on the xdc02 geth archive). `xone snapshot import ... --pins`: `imported block 776785 ... 14
  chunks ... in 1.0s`; then `chain: following from block 776785`.

Verification on each node against archive-h4: `eth_getBlockByNumber(n, true)` equal at the pivot or
anchor, pivot + 1, two later heights and head - 10; `XDPoS_getMasternodesByNumber` at the head equal
(13 masternodes); `eth_getBlockByNumber(firstRetained - 1)` gives the typed -32000 refusal and
`firstRetained` itself is served. Board: all three `active:true`; `syncMode` is `snap` for snap and
fast, `full` for minimal (#123).

RPC sweep before the resync (all five nodes against archive-h4, archive-h4 against xdc02): blocks with
and without transactions by number and hash, transaction, receipt, logs, balance, code, storage, nonce,
masternodes, chainId, net_version are equal inside each window, including `validators`, `validator`
and `penalties`. Differences: the refusal `pivot` field gives the head (#121); `eth_subscribe` gives an
id but no notification on any node (#122); `XDPoS_getCandidates` is refused on both sides with
different messages (no issue). archive-h4 runs `--syncmode full` with no `--gcmode archive` and
refuses state below about head - 4100 (the xdc02 archive serves it).

Mint readiness, borrowed seats, one at a time (mint-c33a.log, mint-00ce.log): every node sealed 3 of 3
eligible turns, each block equal on archive-h4 (hash, miner, child parentHash), then rolled back (0
`--mine` xone processes besides h95, geth unit active, follower loop back, staged key removed).

| Node | Seat | Seals (block hash) | Producer up / rolled back |
| --- | --- | --- | --- |
| snap-h95 | 0xc33a | 776899 0x8b4984c1.., 776912 0x2865357c.., 776925 0x928e78d0.. | 08:58:40Z / 08:59:55Z |
| fast-h213 | 0x00ce | 776952 0x3e0ae6a3.., 776965 0x821fe3b5.., 776978 0x0ec52145.. | 09:01:26Z / 09:02:45Z |
| minimal-h213 | 0x00ce | 777004 0x3c2a01eb.., 777017 0x7fd00d78.., 777030 0x4490cc8c.. | 09:03:27Z / 09:04:42Z |

Round 770296 (index 3, 0x00ce) timed out in the cutover gap before fast-h213 started, while no signer
held the key; this is the known gap of the protocol (stop geth, wait 3 blocks), not a producer miss.

## Resync 2026-11-12, third run (build xOne/v0.1.0-85a75a23-12112026)

Build 85a75a23 is main with PR #126 (XB-36, old bodies served without the import lock). The four
follower nodes (snap-h95, fast-h213, minimal-h213, archive-h4) were swapped to it first (binary as
`xone.new`, STOP, stop by pid, `mv`, restart of the loop with the ethstats env); the live producer
`xone-rs-net2040-h95` (build 1676b87c, pid 476871) was not touched and only read over RPC. Then snap,
fast and minimal were wiped and resynced one at a time. Time is from the start of the loop (minimal:
from the start of `snapshot import`) to node head within 1 of archive-h4 for three polls. Peak RSS is
VmRSS sampled every second.

| Node | Flag | Time to tip | Data moved | Datadir at tip | Peak RSS | Pivot / anchor |
| --- | --- | --- | --- | --- | --- | --- |
| `xone-rs-net2040-snap-h95` | `--syncmode snap` | 11 s (install 3 s) | 2 MB over snap/1 + xpre/1 | 72 MB (78 MB before wipe) | 80,064 kB | pivot 779856 (head 779920 - 64) |
| `xone-rs-net2040-fast-h213` | `--syncmode fast` | 13 s (install 3 s) | 2 MB over snap/1 + xpre/1 | 72 MB | 78,216 kB | pivot 779874 (head 779938 - 64) |
| `xone-rs-net2040-minimal-h213` | `--minimal` after import | 8 s (import 1.0 s) | 4,825,575 B snapshot | 72 MB | 53,872 kB | anchor 779951 |

Mechanism evidence (node.log):

- snap: `--syncmode snap on an empty datadir`; peers 462cba59 (archive-h4), 93229ea4 and a5099978
  share `snap/1 xpre/1`; `pivot block 779856 ... (head 779920 - 64); anchor verified: switch 779736
  0xa4521f83.. with 13 masternodes certifies it by the 3-chain to 779859; pin: peers 0337f630,
  462cba59, 93229ea4 serve switch 779736`; 9532 accounts, 10947 slots, 6 codes in 5 account ranges, 6
  storage ranges, 1 bytecodes, 0 heal requests, every range proven; `state root 0x6b3ccf26..
  verified at the pivot 779856`; 20457 preimages over xpre/1; `executing from 779857`.
- fast: first line `--syncmode fast is an alias of snap, as in geth-XDC: syncing in snap mode`; head
  779938 from 462cba59, 4628 headers linked; window bodies and receipts 778076..=779874 (1799 blocks);
  4 gap blocks with 0x88 proven; same counts, 0 heal; root 0x07c23dc6.. verified at 779874; `executing
  from 779875`.
- minimal: `xone snapshot export` on archive-h4 (loop stopped by pid, export, loop restarted with its
  env): `exported block 779951 0x939a8451..8e981f ... 4,825,575 bytes in 2.6s`, sha256 061aa1ca..
  (equal after the copy through this host). pins.txt `779951 0x939a8451ff8ed6ed458068d3b30e10d0443234fde3510120e2e33591ce8e981f`
  (archive-h4 and the xdc02 geth archive agree). `snapshot import --pins`: `imported block 779951 ...
  14 chunks ... in 1.0s`; `chain: following from block 779951`, 21 blocks imported to catch up.

Verification against archive-h4 (`eth_getBlockByNumber(n, true)` equal at the pivot or anchor, pivot +
1, two later heights and head - 10): all EQUAL on the three nodes. `XDPoS_getMasternodesByNumber` at
the head equal (13 masternodes). `eth_getBlockByNumber(firstRetained - 1)` gives the typed -32000
refusal and `firstRetained` itself is served (snap 778058, fast 778076, minimal 778153). Board: all
three active; `syncMode` snap for snap and fast, `full` for minimal (#123 still open).

RPC sweep on the new build (rpc/sweep.py, five nodes against archive-h4, archive-h4 against xdc02):
blocks by number and hash with and without transactions (including `validators`, `validator`,
`penalties`), transaction, receipt, logs, balance, code, storage, nonce at the head and at retained
heights, `XDPoS_getMasternodesByNumber`, chainId, net_version are equal inside the window. The
typed refusal below the window is the only difference on the follower nodes, as designed. Still open
and already filed: the refusal `pivot` field equals the head (#121, e.g. snap 779928 at head 779926);
`eth_subscribe newHeads` returns an id and delivers nothing within 20 s on every node, the producer
included (#122); minimal board `syncMode` (#123). Not issues: archive-h4 refuses state older than
about head - 4100 (`--syncmode full`, no `--gcmode archive`) while xdc02 serves it;
`XDPoS_getCandidates` is refused on both sides with different messages; `web3_clientVersion` is not
available on the xdc02 geth. No new difference was found, so no new issue was opened.

Mint readiness, borrowed seats, one at a time (mint-c33a.log, mint-00ce.log): 3 of 3 eligible turns
sealed on each node, each block equal on archive-h4 (hash, miner, child parentHash), the node's own
`sealed and sent` line present, then rolled back (0 `--mine` xone besides h95, geth unit active, STOP
removed, follower loop back with its env, staged key and password removed, password and ethstats
secret not found in any log by value).

| Node | Seat | Seals (block hash) | Producer up / rolled back |
| --- | --- | --- | --- |
| snap-h95 | 0xc33a | 780010 0x199b7336.., 780023 0xa205e4c9.., 780036 0xe525c78f.. | 10:59:52Z / 11:01:01Z |
| fast-h213 | 0x00ce | 780063 0xc81b7de1.., 780076 0x8f406d53.., 780089 0x19662857.. | 11:02:46Z / 11:04:01Z |
| minimal-h213 | 0x00ce | 780114 0xf2f438d5.., 780127 0x755d5118.., 780140 0x654f8c96.. | 11:04:28Z / 11:06:48Z |

Three seals are a smoke test, not reliability evidence. Rounds were the seat's turns every 13
rounds (eligible turns 3, seals 3). Stopping geth before the producer starts leaves a gap (72 s on
fast-h213, whose cutover waited for 3 blocks with no 0x00ce signer); minimal-h213 sealed its first
block 83 s after start for the same reason.
