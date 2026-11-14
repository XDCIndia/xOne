# JSON-RPC methods

Generated from the method registries (`crates/xone-rpc/src/policy.rs`) by `crates/xone-rpc/tests/rpc_doc.rs`; do not edit by hand (`UPDATE_DOCS=1 cargo test -p xone-rpc --test rpc_doc` rewrites it). Scope and parity notes: `spec/rpc/scope-adr-008.md`.

A listener serves only the namespaces of its `--http.api`, `--ws.api` (or IPC) list. `eth_subscribe` and `eth_unsubscribe` (`newHeads`, `logs`, `newPendingTransactions`) are served on WebSocket and IPC only.

Class is the ADR-007 cost class: the token-bucket units the call takes and its deadline.

Errors every method can return:

| Code | Meaning |
|---|---|
| -32700 | parse error |
| -32600 | invalid request (envelope, batch or body too large) |
| -32601 | method not found, or its namespace is not enabled on this listener |
| -32602 | invalid params |
| -32603 | internal error |
| -32005 | limit exceeded (rate bucket, class concurrency, node busy); `data` is the retry delay |
| -32000 | server error, including the typed unsupported-history error for data outside the retained window |

## `eth`: Ethereum methods, the XDC extensions served under `eth_`, and the state-heal status

| Method | Class |
|---|---|
| `eth_blockNumber` | C0 (1 units, 2 s) |
| `eth_call` | C1 (5 units, 10 s) |
| `eth_chainId` | C0 (1 units, 2 s) |
| `eth_coinbase` | C0 (1 units, 2 s) |
| `eth_estimateGas` | C2 (20 units, 10 s) |
| `eth_feeHistory` | C2 (20 units, 10 s) |
| `eth_gasPrice` | C0 (1 units, 2 s) |
| `eth_getBalance` | C1 (5 units, 10 s) |
| `eth_getBlockByHash` | C1 (5 units, 10 s) |
| `eth_getBlockByNumber` | C1 (5 units, 10 s) |
| `eth_getBlockFinalityByHash` | C1 (5 units, 10 s) |
| `eth_getBlockFinalityByNumber` | C1 (5 units, 10 s) |
| `eth_getBlockReceipts` | C2 (20 units, 10 s) |
| `eth_getBlockSignersByHash` | C1 (5 units, 10 s) |
| `eth_getBlockSignersByNumber` | C1 (5 units, 10 s) |
| `eth_getBlockTransactionCountByHash` | C1 (5 units, 10 s) |
| `eth_getBlockTransactionCountByNumber` | C1 (5 units, 10 s) |
| `eth_getCandidateStatus` | C1 (5 units, 10 s) |
| `eth_getCandidates` | C1 (5 units, 10 s) |
| `eth_getCode` | C1 (5 units, 10 s) |
| `eth_getLogs` | C2 (20 units, 10 s) |
| `eth_getMasternodes` | C1 (5 units, 10 s) |
| `eth_getStateHealStatus` | C0 (1 units, 2 s) |
| `eth_getStorageAt` | C1 (5 units, 10 s) |
| `eth_getTransactionByBlockHashAndIndex` | C1 (5 units, 10 s) |
| `eth_getTransactionByBlockNumberAndIndex` | C1 (5 units, 10 s) |
| `eth_getTransactionByHash` | C1 (5 units, 10 s) |
| `eth_getTransactionCount` | C1 (5 units, 10 s) |
| `eth_getTransactionReceipt` | C1 (5 units, 10 s) |
| `eth_getUncleCountByBlockHash` | C1 (5 units, 10 s) |
| `eth_getUncleCountByBlockNumber` | C1 (5 units, 10 s) |
| `eth_mining` | C0 (1 units, 2 s) |
| `eth_sendRawTransaction` | C1 (5 units, 10 s) |
| `eth_syncing` | C0 (1 units, 2 s) |

## `xdc`: the `xdc_` prefix geth-XDC serves for the eth block-chain methods

| Method | Class |
|---|---|
| `xdc_blockNumber` | C0 (1 units, 2 s) |
| `xdc_call` | C1 (5 units, 10 s) |
| `xdc_chainId` | C0 (1 units, 2 s) |
| `xdc_estimateGas` | C2 (20 units, 10 s) |
| `xdc_getBalance` | C1 (5 units, 10 s) |
| `xdc_getBlockByHash` | C1 (5 units, 10 s) |
| `xdc_getBlockByNumber` | C1 (5 units, 10 s) |
| `xdc_getBlockFinalityByHash` | C1 (5 units, 10 s) |
| `xdc_getBlockFinalityByNumber` | C1 (5 units, 10 s) |
| `xdc_getBlockReceipts` | C2 (20 units, 10 s) |
| `xdc_getBlockSignersByHash` | C1 (5 units, 10 s) |
| `xdc_getBlockSignersByNumber` | C1 (5 units, 10 s) |
| `xdc_getCandidateStatus` | C1 (5 units, 10 s) |
| `xdc_getCandidates` | C1 (5 units, 10 s) |
| `xdc_getCode` | C1 (5 units, 10 s) |
| `xdc_getMasternodes` | C1 (5 units, 10 s) |
| `xdc_getStateHealStatus` | C0 (1 units, 2 s) |
| `xdc_getStorageAt` | C1 (5 units, 10 s) |
| `xdc_getUncleCountByBlockHash` | C1 (5 units, 10 s) |
| `xdc_getUncleCountByBlockNumber` | C1 (5 units, 10 s) |

## `net`: network identity and peer count

| Method | Class |
|---|---|
| `net_listening` | C0 (1 units, 2 s) |
| `net_peerCount` | C0 (1 units, 2 s) |
| `net_version` | C0 (1 units, 2 s) |

## `web3`: client version

| Method | Class |
|---|---|
| `web3_clientVersion` | C0 (1 units, 2 s) |

## `txpool`: the transaction pool

| Method | Class |
|---|---|
| `txpool_content` | C2 (20 units, 10 s) |
| `txpool_inspect` | C2 (20 units, 10 s) |
| `txpool_status` | C0 (1 units, 2 s) |

## `admin`: node and peer administration (never enabled by default)

| Method | Class |
|---|---|
| `admin_addPeer` | C0 (1 units, 2 s) |
| `admin_addTrustedPeer` | C0 (1 units, 2 s) |
| `admin_nodeInfo` | C0 (1 units, 2 s) |
| `admin_peers` | C1 (5 units, 10 s) |
| `admin_removePeer` | C0 (1 units, 2 s) |

## `debug`: raw data and tracers (never enabled by default)

| Method | Class |
|---|---|
| `debug_getRawBlock` | C1 (5 units, 10 s) |
| `debug_getRawHeader` | C1 (5 units, 10 s) |
| `debug_traceBlockByHash` | C3 (100 units, 30 s) |
| `debug_traceBlockByNumber` | C3 (100 units, 30 s) |
| `debug_traceTransaction` | C3 (100 units, 30 s) |

## `xdpos`: XDPoS consensus state (`XDPoS_` methods)

| Method | Class |
|---|---|
| `XDPoS_getBlockInfoByEpochNum` | C1 (5 units, 10 s) |
| `XDPoS_getCandidates` | C1 (5 units, 10 s) |
| `XDPoS_getEpochNumbersBetween` | C1 (5 units, 10 s) |
| `XDPoS_getLatestPoolStatus` | C0 (1 units, 2 s) |
| `XDPoS_getMasternodes` | C1 (5 units, 10 s) |
| `XDPoS_getMasternodesByNumber` | C1 (5 units, 10 s) |
| `XDPoS_getMissedRoundsInEpochByBlockNum` | C1 (5 units, 10 s) |
| `XDPoS_getRewardByAccount` | C1 (5 units, 10 s) |
| `XDPoS_getSigners` | C1 (5 units, 10 s) |
| `XDPoS_getSignersAtHash` | C1 (5 units, 10 s) |
| `XDPoS_getSnapshot` | C1 (5 units, 10 s) |
| `XDPoS_getSnapshotAtHash` | C1 (5 units, 10 s) |
| `XDPoS_getV2BlockByNumber` | C1 (5 units, 10 s) |
| `XDPoS_networkInformation` | C0 (1 units, 2 s) |
