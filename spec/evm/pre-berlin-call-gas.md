# DELEGATECALL, STATICCALL and new-account gas before the access-list schedule

**Rule.** Until `Eip1559Block` calls are priced as in Istanbul: 700 gas base (EIP-150), the 63/64 rule, no cold/warm split. DELEGATECALL and STATICCALL charge memory expansion and nothing else on top of the base. A CALL that transfers value to an empty account (EIP-158) or, before EIP-158, to a missing account, pays 25,000 gas for the new account. SELFDESTRUCT pays 25,000 when the beneficiary is empty and the contract holds value.

**Source**
- `core/vm/gas_table.go:396-420` the call variants; `:441-453` the new-account charge; `:503-517` DELEGATECALL and STATICCALL; `:519-538` SELFDESTRUCT.
- `core/vm/jump_table.go:318-328` the EIP-150 base prices.
- `core/vm/evm.go:350-353` a call with zero value to an empty account does nothing.

**Networks.** All.

**Rust.** Nothing to add: `SpecId::ISTANBUL` (or `BYZANTIUM` before the first Istanbul block) already prices calls this way. The regression cases `delegatecall-62.4M` and `staticcall-69.4M` replay real transactions to prove it for the XDC window where stock revm would use the Berlin schedule.
