# Gas refund cap is 1/2 of the gas used until `Eip1559Block`, then 1/5

**Rule.** The refund counter is capped at `gasUsed / 2` before `Eip1559Block` and at `gasUsed / 5` from it, although London (EIP-3529) is active earlier. The SSTORE-clear and SELFDESTRUCT refund amounts follow the table, so they also change at `Eip1559Block`.

**Source**
- `core/state_transition.go:1343-1363` `calcRefund`: on XDC the quotient is `RefundQuotientEIP3529` only if `IsEIP1559`.
- `core/vm/eips.go:166` `enable3529`, part of the London set (`core/vm/jump_table.go:208-213`), which XDC replaces by the marker set until `Eip1559Block`.

**Networks.** All.

**Rust.** `XdcRules::max_refund_quotient` is written into revm's `GasParams` by `xone_evm::gas::gas_params`.
