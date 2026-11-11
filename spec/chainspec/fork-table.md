# The fork table is generated from geth-XDC, never typed

## Rule

`crates/xone-chainspec/generated/forks.json` holds, for mainnet, apothem, devnet (5551) and devnet551 (551):

- the chain id;
- every `*big.Int` field of `params.ChainConfig` whose name ends in `Block`, as a decimal string (`null` for geth `nil`), keys sorted;
- `effective`: the first block at which geth's own predicate holds, for the rules that read more than one row (see the Istanbul page and [non-parameter-switches](non-parameter-switches.md));
- `xdpos`: period, epoch, reward, rewardCheckpoint, gap, `foudationWalletAddr` (geth's spelling), maxMasternodesV2, `v2.switchBlock`, `v2.switchEpoch` and every `v2.allConfigs` entry by round (see [v2-config-by-round](v2-config-by-round.md));
- the genesis hash and state root from `Genesis.ToBlock()`.

`conformance/exporter` writes the file by importing the pinned `params` and `core` packages. Run it in `conformance/exporter` with `go run .`; `go run . -check` regenerates in memory and exits 1 with a diff if the committed file differs. Output is deterministic: running it twice gives the same bytes.

`xone-chainspec` loads the file with `include_str!`. The direction is exporter, then generated file, then chainspec; the Rust crate never calls Go.

## Source

- `params/config.go:2012-2101` the `ChainConfig` fields (`*Block` rows at 2015-2036, 2046-2056, 2074-2089, 2099-2101).
- `params/config.go:69` mainnet, `:1031` apothem, `:1594` devnet 5551, `:1647` devnet551.
- `core/genesis.go:1077-1083` `ToBlock`.
- `core/genesis_xdc.go:55-104` the four built-in genesis blocks.

## Networks

All four.

## Rust

- `ChainSpec` rows and `ChainSpec::fork_rows`; `ForkBlock::{At, Unscheduled, Never}`.
- `load(net, json)` returns a typed `Error` for a missing network or row (`MissingNetwork`, `MissingFork`), a non-decimal value (`InvalidValue`), a non-null fork the crate has no field for (`UnmodelledFork`) and any unknown key (`Json`). Nothing defaults.
- Tests: `generated_table_loads_and_matches_genesis`, `damaged_table_is_a_typed_error_not_a_default`, `fork_rows_snapshot`, `xdpos_snapshot`.

## Legacy

geth-XDC pins `Constantinople`, `Petersburg` and `Istanbul` to the Berlin block on mainnet and apothem so that `CheckConfigForkOrder` passes (`params/config.go:65-68`, `:1025-1030`). The raw rows are therefore not the activation blocks; use the `effective` section.
