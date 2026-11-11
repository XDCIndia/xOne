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

## What is not covered

* The store is still the memory model with files: the history (headers, bodies, the transaction index) is
  held in memory and a restart re-executes the blocks above the newest checkpoint, which takes minutes in
  the dense ranges of this chain.
* One peer, one request in flight at a time.
* The geth `StorageWrite` of a slot written and restored within one transaction (geth lists it as a write
  of its original value, the overlay sees no change) has not occurred on this chain; the hash comparison
  would refuse such a block.
