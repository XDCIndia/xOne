# net2040 live follow: xone on the public end-to-end devnet

`xone --network net2040` follows chain 2040 from genesis: it dials the geth-XDC archive peer, imports every
block through header verification and execution on its own state (roots, receipts, gas, bloom and, from
Amsterdam, the block access list hash compared with each header), serves JSON-RPC and reports to the SkyNet
board as `xone-rs-net2040-h95`. Nothing is trusted from the peer that the node does not recompute; a block
that differs is refused and the peer is set aside.

## Host layout

```
/mnt/data/xone-net2040/run.sh     restart-on-exit loop (below); never mines or signs
/mnt/data/xone-net2040/xone       the binary (`xone --version` is logged at each start)
/mnt/data/xone-net2040/node.log   stdout and stderr of every run, with start and exit lines
/mnt/data/xone-net2040/data/      the datadir: xone-chain.json, nodekey, xone/{static,state,head.json}
```

`data/` and not the directory itself is the datadir because the directory already holds the binary as
`xone` and older files of other experiments.

## Command

The board secret is the `ETHSTATS_SECRET` default of `xOneGo/ops/deploy/run-xone.sh`. It is put in the
environment of the loop at launch and reaches only the command line of the process; it is in no file.

```
XONE_ETHSTATS="xone-rs-net2040-h95:<secret>@stats.xdcindia.com:443" \
  setsid nohup /mnt/data/xone-net2040/run.sh >/dev/null 2>&1 </dev/null &
```

`run.sh` runs, and reruns 5 s after any exit:

```
xone --network net2040 --datadir /mnt/data/xone-net2040/data \
     --bootnodes enode://0337f630...a9@135.181.117.109:35071 --port 30303 \
     --http --http.addr 0.0.0.0 --http.port 8545 --http.api eth,net,web3,XDPoS \
     --ws --ws.addr 0.0.0.0 --ws.port 8546 --identity xone-rs-net2040-h95 \
     --metrics --metrics.addr 127.0.0.1 --metrics.port 6060 \
     --ethstats "$XONE_ETHSTATS"
```

To replace the binary: copy the new build to `xone.new`, `mv` it over `xone`, then `pkill -TERM -f
'^/mnt/data/xone-net2040/xone '`. The stop writes a state checkpoint and syncs the segments; the loop starts
the new binary, which opens at the head it stopped at.

## What had to change to follow net2040

Each was found by the follower itself (a refused block or a dropped session), fixed at the cause, and the
block then imported with the roots equal to the archive's header.

| symptom | cause | fix |
| --- | --- | --- |
| the peer session ends every 20 to 30 s with `SubprotocolError` | `NewBlock` from block 21,600 on carries the body's withdrawals list (empty), which the wire types did not have: `ListLengthMismatch` | `BlockBody` and `Block` take the optional trailing withdrawals (`eth/protocols/eth/protocol.go:325-329`, `core/types/block.go:278-283`); the node, the segments and the replay cache keep them, so the body check sees them (`withdrawalsRoot`) |
| 8 blocks per second, one core idle | six fsyncs per block (three segments, directory, `head.json`, directory), about 20 ms each on this disk | group commit: segments and `head.json` are synced at most once a second, at the end of each import batch and before each checkpoint; the ADR-005 order (segments before `head.json`, torn tail cut on open) is unchanged |
| 5 to 9 s per block of 10,000 transfers | sender recovery, about 0.5 ms per signature on the loaded host: k256's `recover_from_prehash` recovers the key and then verifies the signature with it again | `ecrecover` computes the key as libsecp256k1's `secp256k1_ecdsa_recover` does (which geth calls) and stops there, about 3 times faster, checked equal to k256's answer on 2,000 keys in low and high form with every `v`; transaction recovery is split over up to 8 threads and the signatures of a QC over worker threads as geth's `recoverUniqueSigners` does (`engine_v2/utils.go:147-150`), the results and the first error being those of the sequential loop |
| block 11,699: `no second epoch switch below block 11699` | the reward walk of the first V2 epochs crosses the V1 checkpoints, which geth's `IsEpochSwitch` counts (`consensus/XDPoS/xdpos_api.go:228-235`) | the node's history marks every multiple of the epoch up to the V2 switch block as a switch, as the replay harness does |
| block 13,499: state root differs | the reward of the third V2 epoch read the masternodes of a V2 switch from `extraData` | the list of a switch is `extraData` up to the V2 switch block and `validators` above it (`GetMasternodesFromEpochSwitchHeader`, `engine_v2/engine.go:1656-1705`); the standby pool and the burnt fees of the tiered reward come from the history too |
| about 2 blocks per second through the dense ranges, the import thread a third busy | the peer answers a body request with what fits its 2 MB response limit, often one dense block, and the driver then asked for the headers again: two round trips per block, none overlapping the execution | the bodies still missing are asked for before the ones at hand are imported, so the next answer travels while this one executes |
| memory grew by about 1 GB per 1,000 dense blocks | every block was held three times (history, the store model's records and its segment bytes) with the receipts and the state diff | the store model forgets records and segment bytes below the window, receipts below it are read back from `receipts.seg`, and the window is geth's `TriesInMemory`, 128 blocks (`core/state/statedb.go:47`) |
| none yet: Amsterdam transactions other than plain transfers | the executor built the EIP-7928 list for plain transfers only | the overlay records every account and slot a transaction reads, with its value before it; at the end of the transaction each slot is a read or a write of its new value and each balance, nonce or code that differs is a change (`finaliseAmsterdam`, `core/state/statedb.go:854-953`; `state_object.go:186-190`, `:255-285`); checked on 21,600..22,408 (2,315,030 transfers, 321 sign transactions) against the archive's `blockAccessListHash` |

Also added: `--metrics` serves `/metrics`, `/livez` and `/readyz` from the same view as the board (head,
syncing, peers, pending), and a genesis file of a chain id without a built-in spec is run on the spec of its
own `config` block (`ChainSpec::from_genesis_json`; a missing or unknown row is a typed error).

## Verification

Run of 2026-11-12, binary `xOne/v0.1.0-0054e83e-dirty-11112026/linux-x86_64/rustc1.94.0` (branch
`feat/net2040-e2e-opus`), first start at 19:32 UTC from an empty datadir, caught up at 21:57 UTC (the
first 45,000 blocks with the slower builds of the table above, the remaining 714,000 in 80 minutes; 11 starts in all,
each resuming at the head it stopped at or at the newest checkpoint). Checked at 2026-11-11T21:57:47Z:

```
$ curl -s -d '{"method":"eth_blockNumber",...}' http://127.0.0.1:8545     # local
{"id":1,"jsonrpc":"2.0","result":"0xb97c2"}
$ ssh xdc02 curl ... http://127.0.0.1:10071                                 # archive
{"jsonrpc":"2.0","id":1,"result":"0xb97c2"}
local 759746 archive 759746 lag 0

eth_getBlockByNumber [h, false] and XDPoS_getMasternodesByNumber [h], local vs archive, whole JSON
18000  eth_getBlockByNumber equal (0x68e72f4b8a1b61aab2872966088a8383917721da51e27b3fc009fc56d5caf907)  Prague
18000  XDPoS_getMasternodesByNumber equal (10)
21600  eth_getBlockByNumber equal (0xacbb3da137b6fbe6e2d2f4ed0cad6ce1c4dc6f574347bf1daeeef10c9eeec589)  Amsterdam
21600  XDPoS_getMasternodesByNumber equal (6)
177963 eth_getBlockByNumber equal (0xa60cb194097f344e6193dd0a8c6139892ee7720562c4891b8087ab4354338a74)  first 0x88 call under EIP-7928
177963 XDPoS_getMasternodesByNumber equal (6)
525514 eth_getBlockByNumber equal (0xc4d088108728c869473a5d06b20cd433fda917a53ffb2d50b3075dbb891a7345)  resign
525514 XDPoS_getMasternodesByNumber equal (12)
754000 eth_getBlockByNumber equal (0xa88229f5aebbd5cb8461b4f3b3c5d94e9a0d9b3fec46ef369c40c093130e476b)
754000 XDPoS_getMasternodesByNumber equal (13)
```

The SkyNet board (`https://skynet.xdcindia.com/old/stats?net=2040`) a few seconds later:

```
name "xone-rs-net2040-h95"
clientVersion "xOne/v0.1.0-0054e83e-dirty-11112026/linux-x86_64/rustc1.94.0"
blockNumber 759751
blockHash "0x674a0202...07484"      (the archive's block 759,751)
peers 1  syncing false  active true
```

The RPC answers on the public address (`http://95.217.56.168:8545`, `eth_blockNumber` 0xb97c7, `net_version`
2040); `/readyz` on 127.0.0.1:6060 answers `ready`. Resident memory at the head: about 13 GB (the history of
754,000 blocks and 32 million transactions in memory, the state is 5,000 accounts).

Every block was imported only after its header check and its execution on the node's own state, with the
state root, receipts root, bloom, gas used and, from 21,600, the block access list hash equal to the header;
the only refusals in `node.log` are those of the table above (11,699 and 13,499, before the fixes).

## MDBX store

Run of 2026-11-11/12 on `integration/2026-11-11` (#109 with #110 merged): the node store is `MdbxStore`
(`chain.mdbx` plus CRC-32C segment chunks, ADR-005). What changed against the run above:

* the history keeps headers, sign transactions and the transaction index; bodies and receipts only for the
  last 128 blocks (`hist::MEMORY_WINDOW`, geth's `TriesInMemory`); older ones are read from the store
  (`Core::records`) for `eth_getBlock*`, receipts, eth/63 `GetBlockBodies` and the board;
* a restart reads the stored headers, reads every body once for the transaction index, replays the blocks
  above the newest checkpoint and keeps the newest two checkpoints (an older one is outside the unwind
  window);
* group commit carried onto MDBX (`mdbx::Config::group_commit`, a node that does not sign): block commits
  skip the journal, segment and directory syncs and MDBX runs `SafeNoSync`; once a second the segment
  chunks the deferred blocks wrote to are synced and then MDBX is made steady. Without it the commit made
  about a dozen syncs per block (30 ms each on this disk, `dd oflag=dsync`: 3 blocks per second). A node
  with a producer keeps the durable per-block commit; `durability()` is not `Durable` while blocks are
  deferred. Fault test `tests/fault/tests/group_commit.rs`: `kill -9` loses nothing, a power loss returns to
  the last steady point.

Second instance, beside the live node (which was not touched), from an empty datadir:

```
/mnt/data/xone-net2040/mdbx-xone --network net2040 --datadir /mnt/data/xone-net2040/mdbx-data \
    --bootnodes enode://0337f630...a9@135.181.117.109:35071 --port 30305 \
    --http --http.addr 127.0.0.1 --http.port 18545 --http.api eth,net,web3,XDPoS \
    --ws --ws.addr 127.0.0.1 --ws.port 18547 --identity xone-rs-net2040-mdbx
```

(30304 and 18546 were held by outgoing connections of other processes on this host, whose ephemeral range
is 15000-65000.) First start 22:23 UTC with `xOne/v0.1.0-27ebd217`, one restart at block 22,045 to
`xOne/v0.1.0-cc644fb0` (checkpoint retention; it replayed 22,001..=22,045 from the checkpoint of 22,000 and
followed on in 77 s), head of the archive reached at 23:58 UTC: genesis to 762,846 in about 95 minutes.
Checked at 2026-11-11T23:58:30Z, local vs archive (`ssh xdc02 curl ... :10071`, read only), whole JSON:

```
local 762846 archive 762847 lag 1
1      eth_getBlockByNumber equal (0x38bc3bc6c4027502f23a7b0a6103cb311fd64b6488e75a3928a697ef8b0bcfcd)
18000  eth_getBlockByNumber equal (0x68e72f4b8a1b61aab2872966088a8383917721da51e27b3fc009fc56d5caf907)
21600  eth_getBlockByNumber equal (0xacbb3da137b6fbe6e2d2f4ed0cad6ce1c4dc6f574347bf1daeeef10c9eeec589)
177963 eth_getBlockByNumber equal (0xa60cb194097f344e6193dd0a8c6139892ee7720562c4891b8087ab4354338a74)
525514 eth_getBlockByNumber equal (0xc4d088108728c869473a5d06b20cd433fda917a53ffb2d50b3075dbb891a7345)
754000 eth_getBlockByNumber equal (0xa88229f5aebbd5cb8461b4f3b3c5d94e9a0d9b3fec46ef369c40c093130e476b)
762836 eth_getBlockByNumber equal (0x9541313378fe4168b73632a1b3065afcb06dd0f85f35b2f1e1ed2dbeee6a320b)  head-10
762849 eth_getBlockByNumber equal (0xb85d3f243161583880ff39afa3a0c6db7aa16efb2fce79561c69d55369fdd381)  head
762849 XDPoS_getMasternodesByNumber equal (13)
```

No block was refused (`node.log` has no refusal). Resident memory, sampled every 10 s: peak 7.07 GB at the
head (13 GB for the memory store above), 4.5 GB at block 200,000; the rest is the 32 million transaction
hashes of the index and the headers. Datadir at the head: 17 GB (segments, MDBX, two checkpoints).

Restart at the head: SIGTERM at 23:58:46 (stopped in 4 s, state checkpoint of 762,849 written), start at
23:58:49; the store reconciled at its committed head 762,849, the node followed from it after 120 s (the
headers and every body read once), imported the 38 blocks the archive had made meanwhile and reported
`eth_syncing` false; peak 5.7 GB. Then:

```
local 762890 archive 762887
762880 eth_getBlockByNumber equal (0x29c8cc8b58a331beb0d0e59320ea76d6694aae6460bf6181d238cf4ba7888218)
762890 eth_getBlockByNumber equal (0x30ab11aaa7950dd5a8db76da86d2a87f23e49bc29468d84c9664b052c8323924)
762890 XDPoS_getMasternodesByNumber equal (13)
```

The ignored `live_net2040` test (genesis to block 1,000 against the archive peer, on the MDBX store) passed
once on this build: `cargo test --release --locked -p xone-node --test live_net2040 -- --ignored`, 39.4 s.

## Lag

How far the follower is behind the network, measured against the xdc02 archive (port 10071, read
only). A sample every 2 s for 15 minutes: `eth_blockNumber` of the node and of the archive (lag in
blocks, archive minus node), `net_peerCount`, and the timestamp of the node's head against the wall
clock. `node.log` was stamped line by line as it was written, giving each single-block import's time
minus its header timestamp. The board view (`/api/v1/fleet/status`, lag at each report's `lastSeen`
against `/api/v1/stats/chain-tip`) has a coarser resolution: one report every 10 to 50 s.

### Before: `46e8769`, the live node, 2026-11-12 02:54-03:09 UTC

| lag (blocks) | 0 | 1 | 2 | 3-5 | >5 |
| --- | --- | --- | --- | --- | --- |
| samples (445) | 142 (31.9%) | 45 (10.1%) | 22 (4.9%) | 63 (14.2%) | 173 (38.9%) |

The maximum was 23 and the mean 5.5. Head-timestamp lag was p50 12.7 s, p95 42.5 s, maximum 63 s.
There were 7 to 10 peers. Of the 82 single-block imports, the time from the header timestamp to the
import was p50 0.25 s, p90 1.16 s, maximum 22 s. In the same window the follower set peers aside for
60 s 132 times: 109 "advertises a head it does not serve" and 23 "does not know our head". On the
board, 03:13-03:28 UTC, `xone-rs-net2040-h95` sent 28 reports: 17 at 0-1 blocks, 2 at 2, 6 at 3-5 and
3 above 5, median 0.7, maximum 13.7. `geth-net2040-00ce` sent 18: 14 at 0-1, 1 at 2, 3 at 3-5, maximum
3.1.

The same 15 minutes on `xone-rs-net2040-archive-h4` (65.21.71.4, also `46e8769`, 04:16-04:31 UTC,
right after it reached the tip):

| lag (blocks) | 0 | 1 | 2 | 3-5 | >5 |
| --- | --- | --- | --- | --- | --- |
| samples (449) | 140 (31.2%) | 17 (3.8%) | 18 (4.0%) | 60 (13.4%) | 214 (47.7%) |

The maximum was 23 and head-timestamp lag p50 14.2 s. Single-block imports were p50 0.31 s after
their header timestamp. There were 110 and 18 set-asides of 60 s.

### Causes

* A `NewBlock` that extends the head was dropped while any request was in flight. The one-second
  poll keeps a request in flight much of the time, so the block waited for the next poll and, at
  worst, for the next peer. geth-XDC inserts a direct extension at once, whatever its syncer does
  (`eth/handler_eth.go:205-296`).
* Peers that were only "not yet there", or whose last announcement was old, were set aside for 60 s.
  geth-XDC (GP5) sends `NewBlock` only for a block it sealed (`eth/handler_xdc.go:102-150`), so a
  peer's advertised head is its own last block: up to 13 blocks old on net2040. A poll that finds
  nothing above our head from such a peer read as "advertises a head it does not serve", for almost
  every peer in turn. A peer that relays a block before its own import, and a peer one block behind us
  ("does not know our head"), gave the same answers. geth-XDC never counts an empty answer at the tip
  against a peer. It counts a strike only when another peer served the same range, and puts the peer
  on a cooldown after 3 strikes (`eth/sync_xdc.go:165-173`, `:305-311`). With every pollable peer set
  aside, a block whose sealer is not a direct peer is not recovered until a minute runs out: 10-23
  blocks of lag, worst right after a restart.
* Not a cause: the group-commit steady point. It runs inside the commit of a block under the store's
  write lock (`xone-sync` `mdbx/mod.rs:937-940`, `chain/insert.rs:212`), so `eth_blockNumber` waits
  for it. Over 300 calls on the live node it answered in p50 0.5 ms, p99 8 ms, maximum 64 ms; an
  fsync of this disk takes about 10 ms.

The producer branch `6ea75ea` (`b4e2ffe` with the synced guard and branch adoption) already stops
the set-aside for a stale announcement. The fix below also covers the other cases.

### Fix (`8a8fa1a`, `services/chain.rs`)

* A `NewBlock` whose parent is the head is imported even while a request is in flight.
* A header poll keeps the hash it was asked from. The answer must start there; a peer that starts
  anywhere else is still set aside for 60 s. Headers at or below a head that a `NewBlock` moved
  meanwhile are skipped, and so are bodies of blocks that are already canonical.
* An empty answer, or our head alone from a peer that advertises more, sets the peer aside for 4 s
  (`NOT_YET`). The caught-up proof is unchanged: an empty continuation, asked from our current head,
  from a peer that advertises that head.

Tests: `crates/xone-node/tests/follow.rs` `new_block_is_imported_while_a_poll_is_in_flight` and
`a_peer_that_is_not_yet_there_is_asked_again_soon`. Both fail on `46e8769`.

### After

`8a8fa1a` on `xone-rs-net2040-archive-h4`, binary swapped through its loop with the flags unchanged,
04:32:07-04:47:07 UTC. The live node, still on `46e8769`, was sampled in the same window:

| lag (blocks) | 0 | 1 | 2 | 3-5 | >5 | max |
| --- | --- | --- | --- | --- | --- | --- |
| h4, `8a8fa1a` (449) | 428 (95.3%) | 16 (3.6%) | 5 (1.1%) | 0 | 0 | 2 |
| live, `46e8769` (444) | 427 (96.2%) | 7 (1.6%) | 9 (2.0%) | 1 (0.2%) | 0 | 3 |

On h4, 98.9% of samples were at 0-1 blocks and none above 2. Head-timestamp lag was p50 1.7 s and
p95 4.1 s; the maximum, 18.3 s, was the same on both nodes, a slow round of the chain itself. Of the
365 single-block imports, the time from the header timestamp to the import was p50 0.27 s, p90 1.27 s,
maximum 2.28 s. On the board, h4 sent 36 reports: 35 at 0-1 blocks, maximum 1.5. `xone-rs-net2040-h95`
sent 20, all at 0-1. `geth-net2040-00ce` sent 17: 16 at 0-1, maximum 1.5.

The old build followed as well in this window, so the comparison shows the failure is episodic. With
`46e8769`, the 60 s set-asides take all pollable peers in turn, and lag then builds whenever the next
block's sealer is not a direct peer. Both "before" windows show it: 31-32% at lag 0 and a maximum of
23. With `8a8fa1a`, the same answers set a peer aside for 4 s. Polling never stops: there were 723 such
set-asides in the window and no lag above 2.

Restart at the head: SIGTERM at 04:50:53.6 UTC. The node stopped and wrote its checkpoint at once,
the loop started it 5 s later, and it was following from 770,548 at 04:50:59.0. Its first RPC answer,
6.3 s after the SIGTERM, was 770,550, equal to the archive. The next 1 s samples were at lag 0, with
one at 1.

Still open: `eth_syncing` on `8a8fa1a` stays an object. The caught-up proof needs the polled peer to
advertise our head, and GP5 peers' stale announcements rarely do. `6ea75ea` accepts an advertised head
that we already hold; keep that when the branches meet.

## Serving old bodies and receipts (XB-36)

The xOneTS follower (TB-09) imported net2040 from genesis and set the local Rust follower (build
`1676b87`) and the fleet node `462cba59` (build `24e39236`) aside for 60 s each: both timed out on
`GetBlockBodies` for blocks 897 to 1024, the first dense blocks (448,860 transactions in the 128 blocks; the
issue reports up to 20,000 in one).

### Causes

1. **No bound on the answer.** The arm answered every hash it was given (up to 128) with no byte limit. 128 of
   these blocks are far more than the 10 MiB message cap of the transport (`spec/net/eth-versions.md` rule 7);
   geth stops at 2 MiB (`eth/protocols/eth/handler.go:37`, `handlers.go:260-277`). The answer was never
   delivered.
2. **The store lock.** An old block was read through `Chain::with_store`, which takes the store's read lock;
   a commit takes the write lock, and a read waited for it, one lock per block (`chain/mod.rs:147`).
3. **The follower loop.** The request was answered inside the loop that also awaits every import, so an
   import of dense blocks (seconds) delayed every header, body and receipts answer behind it. Header requests
   to a node that was importing took about 5 s.
4. **Skipping instead of stopping.** A hash the node did not hold was skipped, so the later bodies moved
   against their hashes. geth stops at the first one it does not hold, which gives the empty list when it holds
   none (`handlers.go:268-272`; the issue names `eth/handler.go`, the rule is in `eth/protocols/eth/`).

### Change

* `StoreReader::{body, receipts}` (`xone-sync` `mdbx/mod.rs`): one MDBX read transaction and the segment file,
  CRC-checked, no store lock and no import lock. A block that is not canonical, has another hash, or has lost
  its index rows (below a minimal node's retention floor) is `None` at once. `Core::{body, receipts, records}`
  read the memory window of the history and then this reader.
* `bodies_answer` and `receipts_answer` (`services/chain.rs`): the order asked, at most 1024, stop once 2 MiB
  are held, stop at the first block not held. Spec: `spec/net/eth-versions.md` rule 10.
* A `Server` task answers `GetBlockHeaders`, `GetBlockBodies` and `GetReceipts` apart from the follower loop;
  bodies and receipts are read on blocking threads, four at a time.

### Tests

* `crates/xone-node/tests/follow.rs` `old_bodies_are_served_within_the_deadline_without_the_import_lock`: a
  fake peer feeds 400 blocks; our node is asked for the bodies and receipts of blocks 1 to 128 (below the
  memory window) while it imports, then while a writer holds the import lock and the store for 4 s, then for
  128 hashes it does not hold and for a known-unknown-known list. On `3268a8e` the second answer took 4.02 s
  (the lock) and the test fails; now it takes 11 ms (bodies) and 9 ms (receipts) with the lock held, 28 ms and
  21 ms while importing (Apothem vector blocks, small).
* `services::chain::tests::a_peer_that_does_not_hold_the_range_answers_an_empty_list`: empty list, stop at
  the first gap, 2 MiB cut, 1024 cap, bodies and receipts.

### Live measurement

Throwaway client: `follow.rs` `live_old_bodies` (`--ignored`), a fresh identity and no datadir. It finds the
node id from the Pong of a discv4 Ping, dials, asks for the headers 897..1024 and then bodies and receipts of the
128 blocks, each request naming the hashes still missing. 2026-11-12, host load average 30 to 37 on 20 cores.

Before: the live node (`xone-rs-net2040-h95`, 127.0.0.1:30303, build `1676b87`, not stopped or touched, read
through p2p only). Headers 62 ms; the first `GetBlockBodies` for the 128 blocks got **no answer in 120 s**
(run 1) and none in 25 s (run 2).

After: `fix/xb-36-old-bodies` (release build) on a scratch datadir that synced net2040 from the public bootnode
to block 31,966 and was importing at the time; the client asked it for blocks 897..1024 (all in store, none in
the memory window):

| client build | what | requests | slowest answer | whole range | deadline |
| --- | --- | --- | --- | --- | --- |
| release | bodies, round 1 / 2 | 16 / 16 | 100 ms / 169 ms | 1.00 s / 1.21 s | 10 s |
| release | receipts, round 1 / 2 | 27 / 27 | 52 ms / 57 ms | 0.87 s / 0.94 s | 10 s |
| debug | bodies, 3 rounds | 16 each | 2.84, 2.69, 3.11 s | 28.4, 29.3, 28.1 s | 10 s |
| debug | receipts, 3 rounds | 27 each | 0.44, 0.40, 0.43 s | 8.0, 7.0, 8.0 s | 10 s |

The debug rows are the client's own RLP checks in an unoptimised build (the same node answered in 0.1 s to a
release client), kept to show that even so no answer reached the deadline. Header requests to the importing
scratch node took 91 ms, against 4.8 s before the `Server` task.

A 128-hash request is answered with the first blocks up to 2 MiB (here 8 to 16 blocks for bodies), so a client
asks again for the rest, as it does geth. The live node itself has not been updated: it runs `1676b87` until
the owner replaces it.

## What is not covered

* The history still holds every header and the transaction index in memory (about 7 GB at 762,000
  blocks), and a restart reads every body once to rebuild the index (2 minutes here). Persisting them in
  the store's consensus and index tables removes both.
* One peer, one request in flight at a time.
* The geth `StorageWrite` of a slot written and restored within one transaction (geth lists it as a write
  of its original value, the overlay sees no change) has not occurred on this chain; the hash comparison
  would refuse such a block.
