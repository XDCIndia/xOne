# Rules that are not a plain `params` fork row

All `file:line` are at `6fc8e8c2088b7f18264f5663036669d04fa7becf`. "Rust" names the explicit row or `ChainSpec` method that represents the rule; "none" means no representation exists yet, and the item is listed under Gaps.

How this list was made: grep of the pinned tree (not tests, not `cmd/`) for `IsXDC()` (62 hits), `ChainID.Cmp`, `ChainID ==`, `ChainID.Uint64() ==`, `chainID ==`, every chain-id literal (50, 51, 551, 5551), `TIPXDCXCancellationFee`, `xdcTIPBlock`, `CopyXDCConstants`, and `IsEIP1559`/`Eip1559Block` uses. Hits that only guard blobs, txpool, parallel execution or logging are in the last section.

## A. Activation rules that read more than one row or a global

| # | Rule | geth at the pin | Networks | Rust |
| --- | --- | --- | --- | --- |
| A1 | `IsIstanbul` is true at `TIPXDCXCancellationFee` or the Istanbul row | `params/config.go:3027-3029` | 50, 51 earlier than row; 5551, 551 at 0 | `is_istanbul`, `effective.istanbul` |
| A2 | `IsPetersburg` is true at `TIPXDCXCancellationFee`, the Petersburg row, or Constantinople if the row is `nil` | `params/config.go:3022-3024` | same as A1 | `is_petersburg`, `effective.petersburg` |
| A3 | `IsConstantinople` has no fallback (checked: plain row) | `params/config.go:3000-3002` | all | `constantinople` row, `effective.constantinople` |
| A4 | `IsTIPXDCXCancellationFee` reads the config row, else the global `common.TIPXDCX` (not `TIPXDCXCancellationFee`) set by `CopyXDCConstants` | `params/config.go:3343-3345`, `:3280-3287`; `common/types.go:218-219` (51), `:234-235` (50), `:248-249` (551), `:316-317` (5551) | all | `effective.tip_xdcx_cancellation_fee` |
| A5 | `CopyXDCConstants(chainID)` is called at start-up for every XDPoS chain; chain ids 50, 51, 551, 5151, 5050, 5551, 99999 have their own case and any other id gets the mainnet-base default | `eth/backend.go:239-241`; `common/types.go:210-374` | all; 5151, 5050, 99999 are not built-in | `effective.*` (values exported per network) |
| A6 | TIP gates whose value is the config row when set, else a global: `TIP2019`, `TIPSigning`, `TIPNoHalvingMNReward`, `TIPRandomize`, `TIPTRC21Fee`, `Gas50x`, `TIPXDCX`, `TIPXDCXMinerDisable` | `params/config.go:3292`, `:3299`, `:3313`, `:3318`, `:3348`, `:3362`, `:3464`, `:3470` | all (rows are `nil` on mainnet and apothem) | `effective.tip2019`, `tip_signing`, `tip_no_halving_mn_reward`, `tip_randomize`, `tip_trc21_fee`, `gas50x`, `tip_xdcx`, `tip_xdcx_miner_disable` |
| A7 | Genesis `tipSigningBlock` overrides the global `TIPSigning`; genesis `denylistBlock` overrides `BlackListHFNumber` | `eth/backend.go:252-255`, `:263-266`; `params/config.go:3307-3309` | custom genesis; devnet551 sets `tipSigningBlock` | `effective.tip_signing` (devnet551 row is `tip_signing`) |
| A8 | `isBlockForked(nil, n)` is false | `params/config.go:3897-3902` | all | `ForkBlock::Never` |
| A9 | `IsTIPXDCXReceiver` is a window: after `TIPXDCX` and before `TIPXDCXReceiverDisable` | `params/config.go:3477-3480` | all | none (Gap G1) |
| A10 | `IsTIPIncreaseMasternodes` returns false unless the row is set; the real value is `consensus/XDPoS.TIPIncreaseMasternodes` from `SetNetworkConstants` | `params/config.go:3324-3332`; `consensus/XDPoS/network_constants.go:57`, `:85`, `:121`, `:155`, `:185`, `:247`; `consensus/XDPoS/constants.go:70`, `:127` | all | none (Gap G2) |

## B. EIP-1559 decoupled from London (see [eip1559-decoupled-from-london](eip1559-decoupled-from-london.md))

| # | Rule | geth at the pin | Networks | Rust |
| --- | --- | --- | --- | --- |
| B1 | `IsEIP1559` is the `Eip1559Block` row alone; `nil` is false (the comment says London fallback, the code has none) | `params/config.go:3046-3051` | all | `is_eip1559` |
| B2 | EIP-2929 at `Eip1559Block`, not Berlin | `params/config.go:4075-4078` | all | `is_eip2929` |
| B3 | EIP-3860 at `Eip1559Block`, not Shanghai | `params/config.go:4105` | all | `is_eip3860` |
| B4 | Merge and Shanghai rules at London; Cancun at `CancunBlock` | `params/config.go:4102-4104` | all | `london`, `cancun` rows |
| B5 | Prague, Osaka, Amsterdam by block number on XDC | `params/config.go:4108`, `:4112`, `:4116`; `:3141-3145`, `:3152-3156`, `:3188-3192` | all | `prague`, `amsterdam` rows; Osaka is `null` everywhere and the loader rejects a non-null value (`UnmodelledFork`) |
| B6 | EIP-3529 refund cap from `Eip1559Block` | `core/state_transition.go:1351-1357` | all | `is_eip1559` |
| B7 | Signer: type 1 and 2 transactions from `Eip1559Block` | `core/types/transaction_signing.go:52-57` | all | `is_eip1559` |
| B8 | EVM jump table: Shanghai set from `Eip1559Block` | `core/vm/evm.go:185-188` | all | `is_eip1559` |
| B9 | Genesis base fee on `IsEIP1559(0)`, not London | `core/genesis.go:1121-1124` | 5551 (London 0, `Eip1559Block` later) | `Genesis::to_header` uses `eip1559` |
| B10 | `BASEFEE` with a `nil` header base fee returns a tier price | `core/vm/eips.go:228-230` | between London and `Eip1559Block` | `london`, `eip1559` rows (execution not implemented) |
| B11 | `StaticCall` touches the callee only if Istanbul is active on XDC | `core/vm/evm.go:513-515` | all | `is_istanbul` |

## C. Per-chain exceptions in code

| # | Rule | geth at the pin | Networks | Rust |
| --- | --- | --- | --- | --- |
| C1 | 68 gas per non-zero calldata byte before Prague, and at every height on chain 5551 | `core/state_transition.go:76-81` | 50, 51, 551 before Prague; 5551 always | `is_pre_eip2028_calldata` |
| C2 | Multicall3 code installed on Prague blocks after genesis, except chain 5551 | `core/state_processor.go:78-84` | 51, 551 (50 has no Prague row yet) | `installs_multicall3` |
| C3 | Protector and observer pools for the reward come from `legacyStandbyPool` on chain 5551 | `eth/hooks/engine_v2_hooks.go:981-986`, `:1075` | 5551 | `chain_id == 5551`; no predicate, needs state (Gap G3) |
| C4 | V1 full verification and double validation are off on Apothem | `consensus/XDPoS/xdpos.go:1210-1213`, `:1371-1373`; `eth/hooks/hooks.go:77-95` | 51 | `chain_id == 51`; no predicate (Gap G3) |
| C5 | Signer-in-set check skipped at listed V1 checkpoints | `consensus/XDPoS/constants.go:156-184` | 50, 51 | none (Gap G4) |
| C6 | `TIPRandomize` is not set by the 5551 case of `CopyXDCConstants`, so 5551 keeps the mainnet-base value although `devnetConstants` in the XDPoS package says 0 | `common/types.go:308-324`; `consensus/XDPoS/network_constants.go:110` | 5551 | `effective.tip_randomize`; the two sources disagree, see Legacy |

## D. Rules that exist only as a chain-id or `IsXDC` branch with no consensus table value

| # | Rule | geth at the pin | Effect |
| --- | --- | --- | --- |
| D1 | `IsXDC()` is `XDPoS != nil` | `params/config.go:3226-3228` | every built-in is XDC; `ChainSpec.xdpos` is not optional in effect |
| D2 | `PREVRANDAO` is `keccak256(header.Number.Bytes())` on XDC | `core/evm.go:72-74` | execution |
| D3 | XDC chain config must not set `cancunTime`, `pragueTime`, `osakaTime`, `amsterdamTime`; Amsterdam must not precede Osaka | `params/config.go:3585-3600` | validation of a custom genesis; built-ins set none |
| D4 | Registered networks get every `nil` fork filled from the built-in defaults when loaded from a genesis file, future-dated forks included | `core/genesis.go:932-987` | the opposite of Rust, which never defaults |
| D5 | Genesis total difficulty written for XDC | `core/genesis.go:1214-1216` | storage only |
| D6 | Blob fields skipped, parallel executor off, signing-tx cache, UBT off | `core/blockchain_reader.go:240`, `core/state_processor_parallel.go:63`, `core/block_validator.go:187`, `params/config.go:4066` | no table value |
| D7 | `masternodes` RPC response shape differs on mainnet | `consensus/XDPoS/api_extended.go:173` | RPC only |
| D8 | Mainnet is exempt from peer taint and hash blacklisting after an invalid block | `eth/sync_xdc.go:2897-2899` | sync policy only |

## Gaps (not exported yet)

- G1: `TIPXDCXReceiverDisable` window (A9). The exporter finds first-active blocks of monotone predicates; this one rises and then falls.
- G2: `consensus/XDPoS.TIPIncreaseMasternodes` and the other `NetworkConstants` fields (A10), including `BlackListHFNumber` and the V2 switch constants at `network_constants.go:86-97`.
- G3: chain-id branches C3 and C4 have no `ChainSpec` method yet. They belong with the XA-04 and XA-05 consensus items.
- G4: `IgnoreSignerCheckBlocks` maps (C5).
- Networks 5151, 5050 and 99999 have `CopyXDCConstants` cases but are not built-in networks here.

## Legacy

- C6: two parallel constant sets in geth-XDC (`common.CopyXDCConstants`, `consensus/XDPoS.SetNetworkConstants`) disagree for chain 5551 on `TIPRandomize`. the block path reads `ChainConfig.IsTIPRandomize` (`params/config.go:3318`, `eth/hooks/hooks.go:298`), which reads `common.TIPRandomize`; the exported `effective.tip_randomize` is that value. Legacy XDPoSChain behaviour for 5551 is not checked here; owner to adjudicate on chain history.
- A4: the fallback is `common.TIPXDCX`, not `common.TIPXDCXCancellationFee`. The two are equal on every registered chain at the pin, so no value changes today.
