# Balances above 2^256 - 1

**Rule.** The EVM sees the balance modulo 2^256; the account leaf carries the true integer. Transfers, gas purchase, CREATE endowments and SELFDESTRUCT follow `core/state_transition.go:497-557` (`buyGas`) and `core/evm.go:156` (`CanTransfer`, true for a wide sender). Decision and evidence: `spikes/wide-balance/REPORT.md` (XA-02): a journal wrapper with a side map, no revm fork.

**Rust.** `xone_evm::journal::WideJournal` (production version of the spike's option B, with a fast path that calls stock revm when no account is wide) and the wide-sender branch of `XdcHandler::validate_against_state_and_deduct_caller`. A credit above the legal bound (2^264 - 1) is a typed error: `reimburse_caller` and `reward_beneficiary` check the result of `incr_balance`, and credits made inside frames set `WideJournal::fault`.

**Test.** `crates/xone-evm/tests/wide_matrix.rs` runs the 40 rows of the spike's oracle matrix (`spikes/wide-balance/vectors/matrix.json`) through the production journal and handler: balances, storage, transaction outcome and state root equal geth.

**Not done here.** Loading a wide balance from the database, writing it back and forgetting deleted accounts belong to the state layer (XB-07); `SideMap::insert` and `forget` are the hooks.
