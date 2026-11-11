# XDC `eth_*` extensions and the JSON shapes (XB-22)

Oracle: geth-XDC at `conformance/PIN`. Code: `crates/xone-rpc/src/{xdc_ext,json,eth}.rs`.

## Extensions (`internal/ethapi/api_xdc.go`)

| method | geth | answer |
| --- | --- | --- |
| `eth_getBlockSignersByHash` / `ByNumber` | `:58`, `:72` | unknown block, or no masternodes: `[]` with no error. Else the signers of `rpcOutputBlockSigners` (`:292-338`): recovered seals, never the sign transactions. V1: the M2 `validator` seal, then the M1 creator seal in `extraData`; V2: the creator seal in `validator`. Duplicates dropped; with no recoverable seal, the miner field |
| `eth_getBlockFinalityByHash` / `ByNumber` | `:86`, `:100` | `0` for an unknown block or no masternodes. `100` for a canonical V2 block at or below the QC-committed block (`isCommittedV2Block`, F-2026-19638); else `len(signers) * 100 / len(masternodes)`, integer division (`:341-378`). A JSON number |
| `eth_getCandidateStatus(coinbase, epoch)` | `:131` | `{status, capacity, success, epoch}`. Checkpoint = `(epoch / E) * E`, or `E` for 0; `epoch` in the answer = checkpoint / E. Candidate: `status "PROPOSED"` and `capacity` from 0x88; masternode of the checkpoint header: `"MASTERNODE"` (`capacity -1` when it is not a candidate). `capacity` is a JSON number of any size (`*big.Int`), written by `json::bigint`/`render`. No checkpoint block, no candidates or no masternodes: `success false` |
| `eth_getCandidates(epoch)` | `:232` | `{candidates: [{address, capacity, owner}], success, epoch}` sorted by capacity, descending |
| `eth_getMasternodes` | `:114` | **a decoding quirk, kept**: the Go method takes a `*types.Block`, so over RPC every string argument is `-32602 invalid argument 0: json: cannot unmarshal string into Go value of type types.Block` (50 of 50 archive calls). xOne answers the same error |

`eth_mining` and `eth_coinbase` reflect the live validator state (`ChainApi::mining_as`): xOneGo `8733eb54`. The archive node does not serve them (`-32601`).

## Header, block, transaction, receipt, log

`header_json` is `RPCMarshalHeader` (`internal/ethapi/api.go:996`): `number, hash, parentHash, nonce, mixHash, sha3Uncles, logsBloom, stateRoot, miner, difficulty, extraData, gasLimit, gasUsed, timestamp, transactionsRoot, receiptsRoot` and, always, **`validators`, `validator`, `penalties`**; then `baseFeePerGas, withdrawalsRoot, blobGasUsed, excessBlobGas, parentBeaconBlockRoot, requestsHash, blockAccessListHash, slotNumber` when set. A block adds `size` (the length of the block RLP), `transactions` (hashes, or objects when full) and `uncles` (hashes) and, after Shanghai, `withdrawals` (always `[]` on XDC) (`RPCMarshalBlock`, `:1048`). There is no `totalDifficulty` at this pin.

A transaction is `newRPCTransaction` (`:1110`): `type, from, gas, gasPrice, hash, input, nonce, to, value, v, r, s`, the location fields (`null` in the pool), `chainId` for typed and EIP-155 legacy, `yParity` and `accessList` for typed, `maxFeePerGas`/`maxPriorityFeePerGas` for type 2; a mined type 2 transaction's `gasPrice` is `min(tip + baseFee, feeCap)` (`:1204`). The sender is recovered from the signature (`xone_executor::tx::decode`).

A receipt is `MarshalReceipt` (`:1612`): `status` or `root`, `contractAddress` `null` unless the transaction creates (`keccak(rlp([from, nonce]))[12..]`), `effectiveGasPrice` (0 for the 0x89 sign transaction), `logs` with `logIndex` counted over the block.

## `xdc` input

`parse_address` accepts `0x`, `0X`, `xdc`, `XDC` (any case of the three letters) and exactly 40 hex digits (`common/types.go:695-707`). It is the only address parser, so every method that takes an address accepts it: `eth_getBalance`, `getCode`, `getStorageAt`, `getTransactionCount`, `eth_call` (`from`, `to`), `getLogs` (`address`), `eth_getCandidateStatus`, `XDPoS_getRewardByAccount`. Output addresses are always `0x`, lower case.

## Errors

`-32700` parse error, `-32600` invalid request (batch empty or over 100: `batch too large`, one error object), `-32601` method not found, `-32602` invalid params, `-32603` internal, `-32000` handler errors (`header not found`, the unsupported-history error, `request timed out`), `3` for `execution reverted` with the output in `data`, `-32003` response too large, `-32005` `limit exceeded` (ADR-007, `data` is the seconds to wait or `"node busy"`). A notification (no `id`) gets no reply; a batch reply keeps the order and ids of the batch and omits notifications.
