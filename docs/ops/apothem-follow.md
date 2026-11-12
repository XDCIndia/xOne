# Apothem live follow: xone on the public testnet

`xone --network apothem` follows chain 51 from genesis: it dials geth-XDC's Apothem bootnodes, imports
every block through header verification and execution on its own state (state root, receipts root, bloom
and gas used compared with each header; a block that differs is refused and the peer is set aside),
serves JSON-RPC and reports to the SkyNet board as `xone-rs-apothem-h95`. It never mines or signs.

Before it could, the node's history had to stop growing with the chain (section 1): Apothem has about
85 million blocks, and the history of the net2040 run held every header and every transaction hash in
memory (7 GB at 762,000 blocks).

## 1. Bounded history (the prerequisite)

What the history (`crates/xone-node/src/services/hist.rs`) keeps in memory now, and where the rest is:

| what | memory | store |
| --- | --- | --- |
| header, hash, seal signer, sign transactions | the last `window_of(spec)` blocks: 8 epochs (7,200 on Apothem and net2040's 900-block epoch), above the deepest consensus read (5 epoch switches, or 5 epochs of blocks; checked by `hist::tests::window_covers_every_rule` against `Retention::derive` of every network) | headers: `CanonicalHeaders`, `HeaderNumbers` and the header segment, read through `StoreReader` |
| bodies, receipts | the last 128 blocks (`MEMORY_WINDOW`, geth `TriesInMemory`) | the body and receipt segments (as before) |
| transaction index | none | `TxLookup` (new table: tx hash to `block u64 BE || index u32 BE`), written in each block's commit transaction, deleted with the block by an unwind |
| V2 epoch switches | all (56 bytes an epoch: 2 MB at Apothem's head) | `ConsensusEpoch` (hash, round, epoch), loaded at start |
| V2 gap pools | those of the window | `ConsensusSnapshot` (the candidates) |
| V1 snapshots | the last 128 blocks | the state checkpoint (as before) |

`StoreReader` (`crates/xone-sync/src/mdbx/mod.rs`) reads committed rows in its own MDBX read transaction
and a header from its CRC-checked segment frame, checked against the canonical index; it does not take
the store's lock, so a consensus read under execution never waits for the import and the reverse.

A consensus read below the window is not answered with an empty list: sign transactions below the
window are `None`, which the consensus code turns into its typed `Missing` error and the block is
refused (`tests/restart.rs::history_is_a_window_over_the_store` shrinks the window to 300 blocks and
checks that the V1 reward of block 1,800 is refused with `the body of block 700 ... is not available`).

A restart loads the newest state checkpoint, reads the headers of its window from the store, the bodies
and receipts of its last 128 blocks, the sign transactions of the window from the checkpoint (which now
records `signs_from` and the total difficulty), the older epoch switches from `ConsensusEpoch`, and
replays the blocks above the checkpoint. It no longer reads every header and every body. A datadir of
an older build (no `Meta[history]` row) is refused with `CoreError::NoHistoryTables`.

The rows of `ConsensusEpoch` and `ConsensusSnapshot` are written after the block's commit and before any
checkpoint of it, so a restart, which replays from a checkpoint, finds the rows of every block at or
below it; an unwind deletes the rows above its target.

### Proof on net2040 (second instance, from genesis)

Build `xOne/v0.1.0-af707ce7-dirty-12112026` (this branch), beside the live node, which was not touched:

```
/mnt/data/xone-net2040/mdbx2-xone --network net2040 --datadir /mnt/data/xone-net2040/mdbx-data2 \
    --bootnodes enode://0337f630...a9@135.181.117.109:35071 --port 30316 \
    --http --http.addr 127.0.0.1 --http.port 18555 --http.api eth,net,web3,XDPoS \
    --ws --ws.addr 127.0.0.1 --ws.port 18557 --identity xone-rs-net2040-mdbx2
```

(30306 was held by an established loopback connection of other processes, nginx to a node process on
8700; this host's ephemeral range is 15000-65000.) First start 00:45 UTC on 2026-11-12, archive head
reached at 02:33 UTC (genesis to 766,981 in 108 minutes on the shared host, load average 17 to 27, the
workspace tests running beside it). Resident memory sampled every 10 s (`/proc/<pid>/status`):

| block | VmRSS | RssAnon (heap) | RssFile | note |
| --- | --- | --- | --- | --- |
| 200,794 | 3.85 GB | 1.41 GB | 2.44 GB | the run before this change: 4.5 GB |
| 502,077 | 4.19 GB | 1.41 GB | 2.78 GB | |
| 766,981 (head) | 4.21 GB | 1.41 GB | 2.80 GB | the run before this change: 7.07 GB |
| 767,009, after the restart at the head | 0.22 GB | 0.04 GB | 0.18 GB | |

Head over 200,000: 1.09 (VmRSS), 1.00 (RssAnon). The peak of RssAnon over the whole run is 1.50 GB (the
42,000-46,000 and 185,000-230,000 bursts of 10,000-transfer blocks). RssFile is the mapping of
`chain.mdbx/mdbx.dat` (2.9 GB at the head, 2.8 GB of it resident: `/proc/<pid>/smaps`): page cache of the
MDBX file that the B-tree walks touch, which grows with the database (the state tables and `TxLookup`,
33 million rows) and which the kernel reclaims under pressure; the heap does not grow with the chain.
Datadir at the head: 20 GB.

Local against the archive (`ssh xdc02 curl ... :10071`, read only, at most 4 requests per second), whole
JSON, `eth_getBlockByNumber [h, false]` and `eth_getTransactionByHash` of the first and the last
transaction of each block that has any, at 2026-11-12T02:33:18Z:

```
local 766981 archive 766988 lag 7
        1 eth_getBlockByNumber equal (0x38bc3bc6c4027502f23a7b0a6103cb311fd64b6488e75a3928a697ef8b0bcfcd) txs 0
    18000 eth_getBlockByNumber equal (0x68e72f4b8a1b61aab2872966088a8383917721da51e27b3fc009fc56d5caf907) txs 2
          eth_getTransactionByHash 0x2b0b8176236180cb20a323643c0a7767909cbc912fe1e625b2643ef3fda74873 equal (index 0x1)
          eth_getTransactionByHash 0xd569ce8e36f1288b1ebb00a348f0bfa1e2bf04b7a3b831a6f96bbe43a5f7ccd7 equal (index 0x0)
    21600 eth_getBlockByNumber equal (0xacbb3da137b6fbe6e2d2f4ed0cad6ce1c4dc6f574347bf1daeeef10c9eeec589) txs 0
    44000 eth_getBlockByNumber equal (0x14bcd730988c361b8ca80832ee612e8b79cb19b118e332eb2ccb86a47af4db67) txs 4761
          eth_getTransactionByHash 0x91c2dd751ff0ae7d194951989080761df0146effe2f9dc393d5d9e5e3d32d33f equal (index 0x1298)
          eth_getTransactionByHash 0xfad86f4a38253cd7847423c215b027b6018ad89105ad7bedf6dd6a873f5214a7 equal (index 0x0)
   177963 eth_getBlockByNumber equal (0xa60cb194097f344e6193dd0a8c6139892ee7720562c4891b8087ab4354338a74) txs 1
          eth_getTransactionByHash 0xeccc4fe3de555421405d30e0e9cb76929ed7e3ed4955853d5a51d9da98767b19 equal (index 0x0)
   525514 eth_getBlockByNumber equal (0xc4d088108728c869473a5d06b20cd433fda917a53ffb2d50b3075dbb891a7345) txs 2
          eth_getTransactionByHash 0x4d8dad94badf44c825a8ff90fa8a4daefe8249268ff1e077a7eb88bd6ab81e1b equal (index 0x1)
          eth_getTransactionByHash 0x779e6234538117776b39526727e4897f8bc8c9cfad7bd69a94120bbb590ff4b1 equal (index 0x0)
   754000 eth_getBlockByNumber equal (0xa88229f5aebbd5cb8461b4f3b3c5d94e9a0d9b3fec46ef369c40c093130e476b) txs 0
   766971 eth_getBlockByNumber equal (0x77ee382c00fa5ee6c55b3b693dd99c2b52a983d5eeb160bd71616773bb50a645) txs 0
differences: 0
 18000 XDPoS_getMasternodesByNumber equal (10 addresses)
525514 XDPoS_getMasternodesByNumber equal (12 addresses)
```

Every transaction lookup and every block but the last is below the 7,200-block window: they were
answered from `TxLookup`, the canonical index and the segments, and the masternode lists from
`ConsensusEpoch` and the switch headers in the store.

Restart at the head: SIGTERM at 02:33:40.3, `state checkpoint of block 766981 written`, exit 0; the loop
started the binary at 02:33:45 and it reported `following from block 766981` at 02:33:46.8 (the run
before this change: 120 s, the headers and every body read once). It then imported the blocks the
archive had made meanwhile and reported `eth_syncing` false; local 767,009, archive 767,009, lag 0;
766,975 and 767,008 equal. No block was refused in the whole run (`mdbx2-node.log` has no refusal; the 16
peers set aside are `does not know our head` and `advertises a head it does not serve`, peers behind the
node).

## 2. Apothem follower

### Host layout

```
/mnt/data/xone-apothem/run.sh         restart-on-exit loop (below); never mines or signs
/mnt/data/xone-apothem/xone           the binary (`xone --version` is logged at each start)
/mnt/data/xone-apothem/bootnodes.txt  geth-XDC's Apothem bootnodes (ops/bootnodes/apothem.txt)
/mnt/data/xone-apothem/node.log       stdout and stderr of every run, with start and exit lines
/mnt/data/xone-apothem/sample.log     every 60 s: time, pid, VmRSS, RssAnon, RssFile (kB), head, peers
/mnt/data/xone-apothem/data/          the datadir
```

### Command

The board secret is the `ETHSTATS_SECRET` default of `xOneGo/ops/deploy/run-xone.sh`; it is put in the
environment of the loop at launch and reaches only the command line of the process.

```
XONE_ETHSTATS="xone-rs-apothem-h95:<secret>@stats.xdcindia.com:443" \
  setsid nohup /mnt/data/xone-apothem/run.sh >/dev/null 2>&1 </dev/null &
```

`run.sh` runs, and reruns 5 s after any exit:

```
xone --network apothem --datadir /mnt/data/xone-apothem/data --port 30307 \
     --bootnodes "$(paste -sd, bootnodes.txt)" \
     --http --http.addr 0.0.0.0 --http.port 8547 --http.api eth,net,web3,XDPoS \
     --ws --ws.addr 0.0.0.0 --ws.port 8548 --identity xone-rs-apothem-h95 \
     --metrics --metrics.addr 127.0.0.1 --metrics.port 6061 \
     --ethstats "$XONE_ETHSTATS"
```

The node has no built-in boot list: started without `--bootnodes` it had no peer after 3 minutes (its
discv4 loop seeds its table only from `--bootnodes`, `services/net.rs::discovery`). With the 28 bootnodes of geth-XDC (`params/bootnodes.go`
`XDCApothemBootnodes` at 6fc8e8c2) the first peer connected within seconds and 8 were connected after
80 minutes.

### Run

First start from an empty datadir at 01:11:47 UTC on 2026-11-12, build `xOne/v0.1.0-af707ce7-dirty-12112026`.
Block 1,064,000 at 02:34:31 UTC: 214 blocks per second on average over the first million V1 blocks
(sign transactions in the EVM, the V1 checkpoint rewards and the checkpoint lists), on the shared host
with the net2040 trial node and the workspace tests beside it. Resident memory 1.1 to 1.2 GB (RssAnon
0.5 to 0.6 GB). Datadir 1.7 GB at 1,066,000.

Every imported block went through the header check and its execution on the node's own state, with the
state root, receipts root, bloom and gas used compared with its header; `node.log` has no refusal and no
peer was set aside.

SkyNet (`https://skynet.xdcindia.com/old/stats?net=51`, chain id 51; `/api/v1/search?q=xone-rs-apothem`)
at 02:34:44Z:

```
name "xone-rs-apothem-h95"  network "apothem"  chainId "51"  status "active"  isActive true
blockHeight "1064555"  lastHeartbeat "2026-11-12T02:34:43.276Z"
node-advancement: deltaBlocks 1040512 over 4853 s
```

Local against the Apothem archive (`ssh xdc02 curl ... :21105`, geth-XDC
`XDC/v1.17.10-xdc.20-08d25caf`, read only), whole JSON, at 2026-11-12T02:34:47Z:

```
local 1065000 archive 85271479 lag 84206479
   965000 eth_getBlockByNumber equal (0xe325b1325371e7a11c6ea3fab6e985651b328be703e509bb52a39d997d2c0ba4) txs 0
  1015000 eth_getBlockByNumber equal (0x58efab53521bc3fba7a82759d95cc4644fc647f3b8b2a0680067bd9fbd4af3ee) txs 0
  1055000 eth_getBlockByNumber equal (0x7c51d04174c0bf1790d6dc7e88e7268d84290d347e349e265b80f001576f5fda) txs 0
  1064000 eth_getBlockByNumber equal (0x1d6b0852cb097f36ff3e902cec5aaa4e37222c3fd55be28655bc0240bf4ce1e1) txs 0
  1064990 eth_getBlockByNumber equal (0x57f6dde2a4787804542d163a2610a95c19087a8d280dff846223edd9e8dad276) txs 0
differences: 0
local 1065251 archive 85271484 lag 84206233
       16 eth_getBlockByNumber equal (0xac6b2750eb1f6f443d4f289656ba6459730547627fb17a133d6edd7f68a80c3d) txs 1
          eth_getTransactionByHash 0xcf1124156a15c36140a91b9c6bc34ea9bb73c98065757641fcc7bbd9275a0849 equal (index 0x0)
     2187 eth_getBlockByNumber equal (0x8f9b35a629aa77d386a19b15d6ada049ebe54d4a66fd0f86e8d77b674bd89e25) txs 1
          eth_getTransactionByHash 0xc2dd2d912be20b25d10b5ea5c53ae37bb54a8b6737a9ed3190adf012ba045ebe equal (index 0x0)
differences: 0
```

Restart: SIGTERM at 02:35:04.1 while the import was writing the state checkpoint of 1,066,000; the
chain service did not stop within its 1.5 s deadline (`services did not stop within their deadline:
["chain"]`, exit 3). The loop started the binary at 02:35:12; it opened at the committed head
(`following from block 1066000`, the checkpoint of that block, nothing to replay) at 02:35:32 (20 s:
the checkpoint's state and the 7,200 headers of the window, each with its V1 seal recovered), was
connected to its first peer about a minute later and imported on from 1,066,001.

Import rate after the restart (`sample.log`, head and peers each minute): 1,066,512 at 02:36:31,
1,072,885 at 02:40:31 (1 to 2 peers, 27 blocks per second while a release build ran beside it),
1,089,000 at 02:43:32 (100 blocks per second); public RPC `http://95.217.56.168:8547` answered
`eth_blockNumber` 0x10a890 at 02:44 and the board showed `blockHeight` 1,091,600, `status` active.

### What is not covered

* Apothem from genesis to its head (85.27 million blocks) takes days at these rates; the V2 switch
  (56,828,700), the fork rows of 61,290,000 and later, and the tiered and proportional rewards have
  been reached by the replay harness's pivot windows (RUNS.md), not yet by this follower.
* The stop does not wait for a checkpoint being written: a SIGTERM during the state checkpoint of a
  thousandth block ends with exit 3 and the restart opens at that checkpoint (nothing is lost; 20 s).
* The follower has no built-in Apothem boot list; it takes `--bootnodes`.
* The state (flat tables and trie) is still in memory, as before: it grows with the state, not the
  chain (Apothem's state at 83.6 million blocks is 325,043 accounts and 8.6 million slots, RUNS.md).
* Minimal mode keeps its pruned blocks' `TxLookup` rows; a lookup of such a transaction finds no body
  and answers null.
