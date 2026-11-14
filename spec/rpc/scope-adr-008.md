# RPC scope (ADR-008 working assumption) and quotas (XB-22)

ADR-008 is not in `docs/adr/` yet; this page is the working scope of issue #42 with ADR-009 (#23): **no XDCx namespaces**. The fleet enables `eth,net,web3,admin,debug,txpool,XDPoS` (`ItWorksinMyLocal/join.sh:686`). Everything else is `-32601`.

## Served

| namespace | methods |
| --- | --- |
| `eth` | `chainId, blockNumber, syncing, mining, coinbase, gasPrice, maxPriorityFeePerGas, accounts (always empty), feeHistory, getHeaderBy{Number,Hash}, getUncleByBlock{Number,Hash}AndIndex (no uncles on XDPoS: null), getRawTransactionByHash, getRawTransactionByBlock{Number,Hash}AndIndex, getBlockByNumber, getBlockByHash, getBlockTransactionCountBy{Number,Hash}, getUncleCountByBlock{Number,Hash}, getBlockReceipts, getTransactionByHash, getTransactionBy{Block...}AndIndex, getTransactionReceipt, getBalance, getTransactionCount, getCode, getStorageAt, call, estimateGas, getLogs, sendRawTransaction`, the XDC extensions, `eth_subscribe` (`newHeads`, `logs`, `newPendingTransactions`) and `eth_unsubscribe` on WebSocket and IPC |
| `net` | `version, listening, peerCount` |
| `web3` | `clientVersion` (the build's one stamp function). `web3_sha3` is not served: the pinned geth-XDC answers `-32601` |
| `txpool` | `status, content, contentFrom, inspect` |
| `rpc` | `modules`: the namespaces of the transport and `rpc`, each at `1.0` (geth `RPCService.Modules`) |
| `admin` | `nodeInfo, peers, addPeer, addTrustedPeer, removePeer` |
| `debug` | `getRawHeader, getRawBlock, traceTransaction, traceBlockByNumber, traceBlockByHash` with `callTracer` and `prestateTracer` (config `onlyTopCall`, `withLog`, `diffMode`); output as geth (`eth/tracers/api.go:175` `txTraceResult{txHash, result}`; `tracers/native/call.go`, `prestate.go`) |
| `xdc` | off unless the API list names it. The 19 methods of the oracle `BlockChainAPI` that this node serves under `eth_` (`xone-rpc/src/xdc.rs` alias table: the same handlers) and `xdc_getStateHealStatus`; `eth_getStateHealStatus` joins `eth`. The idle answer only (GAP-0058) |
| `XDPoS` | the 14 methods of `xdpos-namespace.md` |

## Decisions asked by the issue

| method | in or out | reason |
| --- | --- | --- |
| `eth_getProof` | **out** (`-32601`) | needs account and storage Merkle proofs over the trie; the node stores flat state and recomputes roots (`xone_state::trie`), with no proof walker. No operator, SkyNet or parity tool reads it. Revisit with a light-client item |
| `eth_feeHistory` | **in**, class C2 | ADR-007 section 4 classes it; wallets call it after EIP-1559 (XDC `eip1559Block`). Integer percentiles only; the next block's base fee repeats the last one's (`ponytail:`) until the fee rules are exposed |
| `debug_trace*` | **in**, class C3, `callTracer` and `prestateTracer` only | the reward vectors and the parity tooling read `debug_traceBlockByNumber` with `prestateTracer` and `diffMode` (`conformance/vectors/reward/build/common.py`). The struct logger and the JS tracers are out: `tracer not found` / `tracer not supported`. `debug_traceCall` and `debug_traceBlock` (raw) are out |
| `eth_newFilter` and the other polling filters | out | WebSocket subscriptions cover the same need; no fleet tool polls filters |
| `XDPoSAdmin_*`, `miner_*`, `personal_*`, `trace_*`, `engine_*`, `XDCx*`, `les_*`, `rpc_*`, GraphQL | out | non-goals of #42, ADR-009 |

## Quotas (ADR-007 Q12-Q14, section 4)

`server::Limits` holds the table's defaults; the node overrides them from `[limits]`.

* Classes: C0 `eth_chainId`, `blockNumber`, `net_*`, `web3_clientVersion`, `txpool_status`, `admin_{nodeInfo,add*,remove*}`, `XDPoS_networkInformation`, `getLatestPoolStatus`, `syncing`, `mining`, `coinbase`, `gasPrice`; C2 `getLogs`, `estimateGas`, `getBlockReceipts`, `feeHistory`, `txpool_content`, `txpool_inspect`; C3 `debug_trace*`; everything else C1 (the default for an unknown method). Cost units 1, 5, 20, 100.
* A bucket per connection and one per transport, 400 burst, 200 per second; a batch costs the sum of its parts; at most 100 items; body 5 MiB; response 20 MiB (`-32003`).
* Class concurrency 32 / 8 / 2 for C1 / C2 / C3 (C0 is bounded by the transport's in-flight limit); deadline 2 s / 10 s / 10 s / 30 s; the cancel flag is set on timeout and when the connection drops.
* HTTP: 256 connections (a 257th is closed), 64 in flight, 128 waiting (a 129th is `429` `Retry-After` and `-32005`). WebSocket: 128 connections, 8 in flight per connection (the connection is not read while full), out-buffer 1,024 messages. IPC: 16 connections, 8 in flight.
* A reject is `-32005 limit exceeded`; `data` is the seconds until the bucket holds the cost. Under load shedding (`Rpc::shed`, set by the node's watchdog) C2 and C3 answer `-32005` with `data: "node busy"`.
* Subscriptions: a subscriber whose queue is full is dropped, never waited for; closing a connection removes all of its subscriptions. There is no replay: events published while a client is away are not delivered (the documented gap). Logs of reorged-out blocks are published first with `removed: true`.
* `xone-node::TokenBucket` cannot be imported (`xone-node` ranks above `xone-rpc` in `ci/layering.py`), so `server::Bucket` repeats its arithmetic; `ponytail:` the primitive now lives in `xone-primitives::limits` (rank 0), so `server::Bucket` can be replaced by it.
