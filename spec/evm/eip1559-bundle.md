# EIP-2929, 3529, 2565, 3860 and 3541 start together at `Eip1559Block`

**Rule.** Berlin, London, Merge and Shanghai are marker forks (see [marker-opcodes](marker-opcodes.md)). The cold/warm access pricing (2929), the reduced refunds (3529), the cheaper MODEXP (2565), the initcode word cost and size limit (3860) and the rejection of code that starts with 0xEF (3541) all start at `Eip1559Block`. The table from there on is the upstream Shanghai table, which is why revm's `SHANGHAI` spec is the right base. The coinbase is warm (EIP-3651) from the same block because `IsShanghai` holds from London.

**Source**
- `params/config.go:4075-4078` `IsEIP2929` is `IsEIP1559` on XDC.
- `params/config.go:4105` `IsEIP3860` is `IsEIP1559` on XDC.
- `core/vm/evm.go:185-188` the Shanghai table is used from `IsEIP1559`.
- `core/vm/evm.go:710-717` EIP-3541 is gated on `IsEIP1559`.
- `core/vm/common.go:34` the initcode size limit; `core/state_transition.go:126-134` the intrinsic initcode word cost.

**Networks.** All. The row is `effective.eip1559` (apothem 71,550,000).

**Rust.** `XdcRules::eip1559_bundle`, `SpecId::SHANGHAI` (then `CANCUN`, `PRAGUE` from their own rows). The vector test `rule_flags_equal_geth_at_every_fork_row` compares the flag with geth's `IsEIP1559`, `IsEIP2929` and `IsEIP3860` at every fork row, on the four built-in networks and on devnet551 with `Eip1559Block = 0` (EIP-1559 active at genesis, xOneGo#643), which the oracle builds as a copy of that config.
