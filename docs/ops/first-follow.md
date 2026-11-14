# First follow: xone on a private geth-XDC chain (loopback)

> Source-review note: the transcript below records the earlier development run at `c51c602` plus working-tree changes. It is preserved as historical author-reported evidence, not a test rerun on current main. The review of `876bb8e9679d3df67aabccea32cfe7047a3284c3` found remaining import/datadir, discovery, RPC and sync regressions; see the [current execution plan](../plans/2026-11-11-execution-plan.md) and [review snapshot](../../.audit/current-review.json). The staged-sync and snap components being merged does not establish their full node wiring, durable progress or public qualification.

`xone --xdcdevnet` (or `--apothem`) is now a runnable follower: it dials its peers, imports every block
it is sent through header verification and execution on its own state, serves JSON-RPC, and resumes at the
same head after a restart. This page records what runs, the exact commands, and the output of the first
run against a geth-XDC node. Loopback only: no public network is addressed, no key or host of the fleet is
used.

## What runs

`xone-node::services::build` returns a `Node` of four services, started in this order and stopped in
reverse, each stop bounded (`[limits] service_stop_timeout_ms`, `stop_timeout_ms`, 5 s in all):

| service | what it does |
|---|---|
| `net` | owns the tokio runtime; `xone_net::Peers` with a TCP accept loop; a redial loop per `--bootnodes`, `static-nodes.json` and `trusted-nodes.json` entry; the discv4 UDP loop (ping boot nodes, `findnode` every 5 s, dial what the table learns) unless `--nodiscover`; the ADR-007 queues are those of `Peers` |
| `chain` | the live-follow driver (below), and a state checkpoint on stop |
| `pool` | `xone_builder::txpool::Pool` fed by `Transactions` gossip and `eth_sendRawTransaction`, reset on every new head; serves `PoolApi` |
| `rpc` | the five `xone-rpc` traits over the live chain, and HTTP, WebSocket and IPC per the config; only the namespaces of `--http.api` and `--ws.api` are registered |

The chain is `xone_sync::chain::Chain` over a `FileStore` (the in-memory `MemStore` plus the files below),
with `BlockExecutor` = `xone_executor::execute_block` and `HeaderCheck` = `HeaderVerifier`, both reading the
node's own history (V1 context from the snapshot, V2 context from the gap pools and the epoch switches of
the chain, as the replay harness does for V1). Nothing a peer says is stored before it is verified and
executed, and a block whose roots differ from its header is refused.

### The live-follow driver (replaced by the XB-14 pipeline)

* The best peer by TD rank is asked for `GetBlockHeaders` from our head hash. The first header it returns
  must be our head (a peer that does not know it is put aside for 60 s); the rest are checked to link, their
  bodies are requested, and the blocks are imported in order. An empty continuation is the caught-up proof:
  the peer's chain holds our head and nothing above it (hash equality).
* `NewBlock` that extends the head is imported at once; `NewBlockHashes` and a 1 s tick poll.
* Receipts are not requested: every block is executed and its receipts root compared with the header.
* The node answers `GetBlockHeaders` and `GetBlockBodies` from its history.

### Files under `<datadir>/xone/` (ADR-005 shaped)

```
static/{headers,bodies,receipts}.seg   frame: len u32 | block u64 | payload | 4-byte keccak tag; fsynced
head.json                              head and the committed length of each segment (tmp, fsync, rename)
state/ckpt-<n>.json.gz                 the replay harness's checkpoint, every 1000 blocks and on stop
```

A restart reads `head.json`, cuts torn bytes beyond the committed lengths, loads the newest checkpoint that
belongs to the stored chain, opens the chain on it (`Chain::open` / `resume`) and replays the blocks above
it through `Chain::insert`. A damaged frame, a short segment, a checkpoint that does not give its state
root, or a chain that does not start at this genesis is refused; there is no path that opens at block 0 on
a datadir that holds blocks. The XB-19 marker (`xone-chain.json`) refuses a datadir of another chain first.

## Commands

geth-XDC, from the pinned tree (`conformance/vectors/net/oracle/PIN`, `6fc8e8c2`; its root holds stray `.go`
files that must be removed from a `git archive` for `go build`):

```
GOTOOLCHAIN=local GOPROXY=off go build -o geth-xdc ./cmd/geth        # Go 1.25.2
```

The whole run is `crates/xone-node/tests/live/run.sh` (the ignored test `tests/live.rs` wraps it):

```
XONE_GETH_BIN=geth-xdc XONE_HOLDER=<local_geth test binary> WORK=/tmp/xl \
  bash crates/xone-node/tests/live/run.sh
```

It runs `crates/xone-net/tests/local-geth/run.sh MODE=prepare` (a private devnet 5551 chain, one signer, `--nodiscover
--netrestrict 127.0.0.0/8`), restarts geth with the same flags and `--mine`, then:

```
xone --xdcdevnet --override.genesis $WORK/geth/genesis.json --datadir $WORK/xone \
     --bootnodes <enode> --nodiscover --port 0 --http --http.port <free> --http.api eth,net,web3,XDPoS
```

`--override.genesis` is needed because `run.sh` writes the genesis with its own signer in `extraData`, so its
hash is not the built-in devnet's. `--http.api` names `XDPoS`, which geth does not serve over HTTP by default
either.

## Transcript

Run of `crates/xone-node/tests/live/run.sh` on 2026-11-11, xone built from `feat/node-wiring` at `c51c602` plus
this change (hence `-dirty` in the stamp), geth-XDC `XDC/v1.17.10-xdc.21` from the pin, both on 127.0.0.1.
`PREPARE=12 FOLLOW=50`, ports picked free by the script.

```
[live] work dir /tmp/xp/live, geth p2p port 55873, xone http port 52617
[live] phase 1: run.sh MODE=prepare, 12 blocks
prepared: head 12
[live] prepared chain, enode enode://b2db66d4b4e5121fb91ff0f38f11a38ca143285985a92043071934b29b9a2d0230bb01e56b3f912f8709645ff9d04357d9fabafe3ab1b92b8a5ca2718da7c134@127.0.0.1:55873
[live] phase 2: geth restarted with the same flags and --mine
[live] geth head at restart: 12
[live] xone started: chain: following from block 0 0x122cf8d34e821f19deb93a6ec60b8bd10da48cb7aea234d404ac02c905c7485a
[live] connected and imported: xone head 12, geth head 12
[xone] chain: peer b2db66d4 connected, eth/101, rank 13
[xone] chain: imported 12 blocks, head 12
[live] following 50 more blocks, head lag sampled every second
[live]   geth stalled at 37, restarting it (restart 1)
[live]   geth stalled at 55, restarting it (restart 2)
[live] geth mined 50 blocks (head 12 -> 62, 2 restarts), 432 samples, max head lag 0
[live] RPC answers at block 0x3e (xone over HTTP, geth over IPC)
[live]   eth_blockNumber xone "0x3f" geth "0x3f"
[live]   eth_getBlockByNumber ["0x3e", false]: equal
[live]   eth_getBlockByNumber ["0x0", true]: equal
[live]   XDPoS_getMasternodesByNumber ["0x3e"]: equal
[live]   web3_clientVersion = xOne/v0.1.0-c51c6022-dirty-11112026/linux-x86_64/rustc1.94.0 (equals xone --version)
[live] phase 3: geth stops mining, SIGTERM, restart
[live] SIGTERM: stopped in 0.11s, exit code 0, head 64
[xone] chain: imported 1 blocks, head 64
[xone] chain: state checkpoint of block 64 written
[xone] stopped
[live] restart: chain: following from block 64 0x9ff76fe06c7c8768b9d15b2d35c2fe3129e347bd7f2274e91ea9122976d41ec0
[live] restart head 64 (before the stop: 64)
[live] PASS
```

xone's own log of the first run (`$WORK/xone.1.log`) and of the restart (`$WORK/xone.2.log`):

```
started chain 5551 genesis 0x122cf8d34e821f19deb93a6ec60b8bd10da48cb7aea234d404ac02c905c7485a
chain: following from block 0 0x122cf8d34e821f19deb93a6ec60b8bd10da48cb7aea234d404ac02c905c7485a
rpc: http listening on 127.0.0.1:52617
chain: peer b2db66d4 connected, eth/101, rank 13
chain: imported 12 blocks, head 12
chain: imported 1 blocks, head 13
chain: imported 1 blocks, head 14
... (52 more one-block imports, one per geth block) ...
chain: state checkpoint of block 64 written
stopped
```
```
started chain 5551 genesis 0x122cf8d34e821f19deb93a6ec60b8bd10da48cb7aea234d404ac02c905c7485a
chain: following from block 64 0x9ff76fe06c7c8768b9d15b2d35c2fe3129e347bd7f2274e91ea9122976d41ec0
rpc: http listening on 127.0.0.1:52617
chain: state checkpoint of block 64 written
stopped
```

Read from it:

* connected: one eth/101 session to geth, rank (TD) 13 at the first sync;
* imported 12 blocks on the first poll, every one executed on xone's own state with the roots recomputed
  and compared with geth's headers (a block that differs is refused, none was);
* followed 50 more blocks (head 12 to 62) with a head lag of 0 at each of 432 one-second samples, limit 2.
  This geth mines in bursts of about 17 blocks and then stops until it is restarted (its miner is gated by
  its own syncer; the gate returns whatever the peer does), so the script restarted it twice. xone redialled
  it each time without help;
* `eth_blockNumber`, `eth_getBlockByNumber` (block 62 without and block 0 with transactions) and
  `XDPoS_getMasternodesByNumber` answered over HTTP are byte-equal to geth's answers over its IPC;
  `web3_clientVersion` is the `xone --version` stamp (geth's own answer is its own client string);
* SIGTERM: stopped in 0.11 s, exit 0, a checkpoint of the head written; the restart opens at block 64, the
  head it stopped at, not at 0.

## What is not covered

* MDBX store (the datadir is a memory model with files; start-up replays), the XB-14 pipeline in place of the
  driver, snap sync, and a follow of the public Apothem (needs an approved host).
* One canonical chain: a block whose parent is not the head is refused by the executor, so no reorg reaches
  the store.
* V1 only is exercised. The V2 header context and the V2 RPC reads are written from the consensus crate's
  rules but no V2 chain was available to run them. The executor pays V2 checkpoint rewards (XA-05), but this
  node's context holds no gap snapshots or burn records yet, so it refuses a V2 checkpoint block.
* `eth_call`, `eth_estimateGas`, tracing and `XDPoS_getMissedRoundsInEpochByBlockNum` answer a typed refusal;
  `newPendingTransactions` publishes what the pool makes executable (hashes, or objects with `true`); CORS and virtual hosts are not enforced; the pool is not announced to peers.
* TRC21 token fees: executed as geth (a transaction to a token the issuer lists takes the token-fee path); none
  is listed on this chain, so the path is empty there.

