# The `XDPoS` RPC namespace (XB-22)

Oracle: geth-XDC at `conformance/PIN` (`6fc8e8c2`). Code: `crates/xone-rpc/src/xdpos.rs`. Vectors: `conformance/vectors/rpc/<network>/XDPoS_*.jsonl`, recorded from the archives by `conformance/vectors/rpc/build/build.py` and replayed by `conformance/rpc-parity`.

## Names

The namespace is registered as written, `XDPoS`, and a method is `lowerFirst(Name)` (geth `rpc/service.go` registers by the service namespace and the method name with its first letter lowered). Lookup is exact: `xdpos_getSigners`, `XDPOS_getSigners`, `XDPoS_GetSigners` and `XDPoS_getsigners` are `-32601` `the method <name> does not exist/is not available` (`rpc/errors.go:78-82`; archive vector `xdpos_getSigners.jsonl`, 51 of 51 calls). The method set is exactly (`xdpos::METHODS`, enumerated by a test):

| method | geth | params | answer |
| --- | --- | --- | --- |
| `getSnapshot` | `api.go:41` | `number?` (nil and `latest` are the head) | the V1 snapshot `{number, hash, signers: {addr: {}}, recents: {n: addr}, votes, tally}`; `votes` is `null` when empty. Unknown number: `-32000 unknown block` (`xdpos.go:110`) |
| `getSnapshotAtHash` | `api.go:55` | `hash` | the same |
| `getSigners` | `api.go:64` | `number?` | the snapshot's signers, ascending by address (`Snapshot.GetSigners`) |
| `getSignersAtHash` | `api.go:82` | `hash` | the same |
| `getMasternodes` | `api.go:95` | `number?` | `GetMasternodes(chain, header)`: the list of the epoch of the header |
| `getCandidates` | `api.go:184` | `number?` | the snapshot's signers, as `getSigners` (the geth body is the same) |
| `getV2BlockByNumber` | `api_extended.go:119,138` | `number?` | `{hash, number, round, masternodes, encodedQC}`; unknown block: `{hash: 0x0, number: null, round: 0, masternodes: null, encodedQC: "", error: "block not found"}`. Above the switch block: round, masternodes and the QC of the header (RLP, base64). At or below it: `round 0`, `masternodes null`, `encodedQC ""` |
| `getMasternodesByNumber` | `api_extended.go:171,183` | `number?` | PascalCase object: `Epoch, Number, Round, MasternodesLen, Masternodes, PenaltyLen, Penalty`, then `ProtectorLen/Protectornodes`, `ObserverLen/Observernodes`, `StandbynodesLen/Standbynodes` only when non-empty (the tier split applies from `TIPFixedReward` or `TIPProportionalReward`, `params/config.go:3404`), then `Error` (`null`). `Epoch = switchEpoch + round / epoch`. Chain id 50 returns the older shape: no `Epoch`, and `Standbynodes` always present. V1 blocks: empty lists and `Round 0` |
| `getBlockInfoByEpochNum` | `api_extended.go:250` | `epoch` (uint64, a JSON number) | from `switchEpoch` on: `{hash, round, firstBlock, lastBlock}` from the V2 engine; below it: `{hash: 0x0, round: epoch*E, firstBlock: epoch*E, lastBlock: epoch*E+E-1}` |
| `getEpochNumbersBetween` | `api_extended.go:285` | `begin, end` | begin or end unknown: `null`. `begin` above the switch block: more than 1000 epochs apart is `-32000 range spans N blocks, exceeding the 1000-epoch limit of M blocks`; otherwise the epoch switch block numbers, **`[]` when `begin == end`** (CertiK XDC-30). At or below the switch: `-32000 not supported in the v1 consensus` |
| `getMissedRoundsInEpochByBlockNum` | `api_extended.go:328` | `number?` | V2: `{EpochRound, EpochBlockNumber, MissedRounds}` (`MissedRounds` is `null` when none; rows are `{Round, Miner, CurrentBlockHash, CurrentBlockNum, ParentBlockHash, ParentBlockNum}`); V1: the epoch start for both, `MissedRounds null` |
| `getLatestPoolStatus` | `api_extended.go:352` | none | `{votePool: 0, timeoutPool: 0}` (the pinned source does not read the pools) |
| `networkInformation` | `api_extended.go:360` | none | `{networkId, xdcValidators, epoch, v2SwitchBlock}`; `xdcValidators` is the V2 `MaxMasternodes` of the current config, 18 without V2 |
| `getRewardByAccount` | `api_extended.go:395` | `account, begin, end` | reads reward files of `--store-reward`; without the folder: `-32000 reward folder not configured; start node with --store-reward to enable reward history` (archive vector). xOne does not write reward files; `ConsensusApi::reward_by_account` returns that error until a reward store exists |

Not served: `getV2BlockByHash`, `proposals`, the `XDPoSAdmin` namespace (`api.go:109-150`, `Propose`/`Discard`): not in the 13-method fleet set of xOneGo `pkg/rpc/xdposapi`, and `Propose` writes validator state.

## Findings from the archive

* `getSigners`, `getSnapshot` and `getCandidates` answer `-32000 invalid signature recovery id` or `extra-data 65 byte suffix signature missing` on every V2 block and on many V1 blocks of the archives (net2040: 33 of 50). That is geth-XDC's V1 snapshot walk applied to blocks it cannot verify; the rows are in the vectors. xOne's `ConsensusApi::snapshot` is the backend's to implement; the sweep compares it row by row and reports each difference as `differ`, not as a pass.
* The archive node answers `-32601` for `txpool_*`, `eth_mining`, `eth_coinbase`, `web3_sha3` (those namespaces or methods are not enabled or not present). xOne serves `txpool_*`, `eth_mining` and `eth_coinbase` (the fleet needs them, xOneGo `8733eb54`) and does not serve `web3_sha3`. The sweep marks the first two `not-backed` (no oracle value).

## Input

`rpc.BlockNumber` parameters accept `latest`, `pending`, `safe`, `finalized` (all the head here), `earliest`, a quantity (`0x` and no leading zero) and nothing else; an address accepts `0x` and `xdc` (`common/types.go:695-707`).

## Out of retention

A block whose class is below the retained window (`Retention::is_within`, XB-14) is `-32000` `unsupported history: <class> of block N is below the retained history (...)`, with `data: {class, block, firstRetained, pivot}`. The V2 queries read `GapSnapshot` and header classes; `RpcMasternodes`, `RpcMissedRounds` and `RpcGetLogs` are the `Rule`s in `bootstrap::retention`.
