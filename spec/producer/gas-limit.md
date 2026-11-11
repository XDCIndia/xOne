# Gas limit and base fee of a built block (XB-23)

## Rule

The gas limit of block `n`, child of a block with gas limit `P`, `ceil` the miner's static ceiling:

| `dynamicGasLimitBlock` row | `n` | gas limit |
|---|---|---|
| absent (`nil`, mainnet) | any | `CalcGasLimit(P, max(P, 420,000,000))` (the plateau rule: never aim below the parent) |
| set, or `Unscheduled` (geth holds the far block) | below the row | `ceil` |
| set | at or above the row | `CalcGasLimit(P, ceil)` |

`CalcGasLimit(P, d)`: `delta = P / 1024 - 1` (Go `uint64`, wraps), `d = max(d, 5000)`; below `d`: `min(P + delta, d)`; above: `max(P - delta, d)`.

The base fee in the header is the fixed tier of the block's own number (`xone_evm::basefee`): 12.5 gwei from `Eip1559Block`, 625 gwei (the 2500x tier) from `Gas2500xBlock`, none before. A producer that reads the row at the parent's number is one block late at the row.

## Source

`miner/xdpos_worker.go:320-338` `xdcGasLimit`, `xdcGasCeil`; `core/block_validator.go:266-287` `CalcGasLimit`; `params/protocol_params.go:26-37`; `params/config.go:3370` `IsDynamicGasLimit`; `consensus/misc/eip1559/eip1559.go:121-127` `CalcBaseFee`; `params/xdc_gas.go:31-42`. Rows: Apothem 83,600,000 (both), net2040 16,200 (`dynamicGasLimitBlock`) and 12,600 (`gas2500xBlock`), mainnet none.

## Rust

`xone_builder::producer::gaslimit::{gas_limit, calc_gas_limit, plateau_ceil, base_fee}`.

## Vectors and mutants

`conformance/vectors/producer/oracle.json`, made by `crates/xone-builder/oracle/run-producer.sh` (a throwaway Go program at the pin: the real `xdcGasLimit` through an overlay in package `miner`, the real `CalcBaseFee`): 630 cases, five chains (Apothem, mainnet, net2040's rows, an `Unscheduled` row, no row), the row minus 1, at, plus 1, seven parent limits, three ceilings. Mutants: the row read at the parent's number, the plateau rule aiming at the ceiling, the base fee tier of the parent.

## Not covered

`--miner.gaslimit` is not a flag: the ceiling is 420M.
