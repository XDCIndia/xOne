# BASEFEE, PREVRANDAO and PUSH0 are live from `londonBlock` with Istanbul pricing

**Rule.** From `londonBlock` until `Eip1559Block` geth-XDC runs the Istanbul table plus three opcodes: BASEFEE (London marker), PREVRANDAO in place of DIFFICULTY (Merge marker) and PUSH0 (Shanghai marker). Nothing else of those forks is on: no access lists in the gas schedule, no EIP-3529. The Merge and Shanghai predicates are driven from `londonBlock`, not from `mergeBlock` or `shanghaiBlock`.

While the header has no base fee (London to `Eip1559Block`) BASEFEE returns `XDCBaseFee` (12.5 gwei), pinned to the Gas50x tier.

**Source**
- `params/config.go:4102-4103` `isMergeRule` and `isShanghai` are `IsLondon`.
- `core/vm/jump_table.go:81-84` the four marker sets; `:89-132` their construction.
- `core/vm/evm.go:189-210` which marker set is used at which rule.
- `core/vm/eips.go:222-232` BASEFEE with a missing header base fee; `params/xdc_gas.go:44-50` `XDCBaseFeeForOpcode`; value `params/protocol_params.go:175`.

**Networks.** All. Apothem: 61,290,000 to 71,550,000.

**Rust.** `XdcRules::marker_opcodes`. `xone_evm::handler::instructions` installs `PUSH0`, `BASEFEE` and `0x44` (as PREVRANDAO) over the `ISTANBUL` table; `XdcRules::block_env` sets `BlockEnv::basefee` for BASEFEE. Regression cases `push0-64.0M` and `prevrandao-68.7M`.
