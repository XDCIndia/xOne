# 68 gas per non-zero calldata byte before Prague, 16 from Prague; the 5551 devnet keeps 68

**Rule.** `IntrinsicGas` is called with a rules copy whose `IsIstanbul` is false while `XDCPreEIP2028Calldata` holds, so a non-zero byte costs 68 gas (zero bytes 4). `XDCPreEIP2028Calldata` is true before Prague, and at every height on chain 5551, whose sealers activate Prague and keep 68. The EIP-7623 floor does not read this rule.

**Source**
- `core/state_transition.go:71-80` `XDCPreEIP2028Calldata`.
- `core/state_transition.go:84` `IntrinsicGas`; `:112-116` the non-zero byte price; `:128-134` the initcode word cost.
- `core/state_transition.go:803-806` the rules copy.

**Networks.** 50, 51, 551 before Prague; 5551 always.

**Rust.** The predicate lives in the chain spec: `ChainSpec::is_pre_eip2028_calldata`. `XdcRules::non_zero_calldata_gas` carries 68 or 16 into `gas_params`, and `XdcHandler::validate_initial_tx_gas` prices the floor from the stock schedule. The vector test `intrinsic_gas_equals_geth_at_every_fork_row` compares 6 payloads at every fork row of the four networks with `core.IntrinsicGas`.
