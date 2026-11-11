# Istanbul and Petersburg fire at `TIPXDCXCancellationFee`

## Rule

`IsIstanbul(n)` is true if `IsTIPXDCXCancellationFee(n)` or the `IstanbulBlock` row is reached. `IsPetersburg(n)` is true if `IsTIPXDCXCancellationFee(n)`, or the `PetersburgBlock` row is reached, or `PetersburgBlock` is `nil` and Constantinople is reached. `IsConstantinople` has no such clause.

`IsTIPXDCXCancellationFee(n)` reads `TIPXDCXCancellationFeeBlock` from the chain config when it is set. When it is `nil` it reads the process global `common.TIPXDCX` (not `common.TIPXDCXCancellationFee`; the two hold the same value on every registered chain). That global is set by `CopyXDCConstants(chainID)`, which eth start-up calls for every XDPoS chain.

So on mainnet and apothem the `IstanbulBlock` row is a placeholder aligned with Berlin so that the fork ladder is ordered. Istanbul (EIP-1884 SLOAD pricing, EIP-2200 SSTORE) is really active from the `TIPXDCXCancellationFee` block, which is earlier. Using the raw row would execute about 37.5 to 38 million blocks with the wrong gas rules on each network.

The values are exported as `effective.istanbul`, `effective.petersburg`, `effective.constantinople` and `effective.tipXDCXCancellationFee`. The exporter finds them by evaluating the geth methods, after resetting the globals (`CopyXDCConstants(0)`, the default profile) and applying the network's own case, because some chain cases leave globals untouched and values would otherwise leak from one network to the next.

## Source

- `params/config.go:3022-3024` `IsPetersburg`; `:3027-3029` `IsIstanbul`; `:3000-3002` `IsConstantinople` (no fallback).
- `params/config.go:3343-3345` `IsTIPXDCXCancellationFee`; `:3280-3288` `xdcTIPBlock` (config value, else global).
- `common/types.go:210` `CopyXDCConstants`; the fallback global `TIPXDCX` is set at `:219` (51), `:235` (50), `:249` (551), `:317` (5551).
- `eth/backend.go:239-241` the start-up call.
- `params/config.go:1025-1030`, `:1040` the apothem comment ("effective Istanbul activation via TIPXDCXCancellationFee"); `:65-68`, `:78` mainnet.
- `core/vm/evm.go:513-515` `StaticCall` touches the callee only when Istanbul is active on XDC.

## Networks

mainnet (50) and apothem (51): the effective block is earlier than the row. devnet (5551) and devnet551 (551): effective block 0, equal to the row.

## Rust

- `ChainSpec::is_istanbul`, `ChainSpec::is_petersburg`, `ChainSpec.effective`.
- Tests: `istanbul_and_petersburg_fire_at_tipxdcx_cancellation_fee` (minus one, exact, plus one at the effective block, and at the raw row, for 50 and 51), `derived_rules_equal_what_geth_exported` (the Rust predicates equal geth's exported effective block around every block in the table, on all four networks).

## Legacy

geth-XDC says it mirrors legacy `config_forks.go`, which gates Petersburg on `TIPXDCXCancellationFeeBlock` (`params/config.go:3010-3012`). Not re-verified against legacy source here.
