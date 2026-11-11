# The transaction fee goes to the coinbase, or after `TIPTRC21Fee` to the coinbase owner

**Rule.** Before `TIPTRC21Fee` (strictly greater than the block) the coinbase receives `gasUsed * (gasPrice - baseFee)`; the base fee is subtracted only when the header has one (from `Eip1559Block`). After it the owner of the masternode (`validatorsState[coinbase].owner` in the validator contract 0x88, read from the state at the start of the block) receives `gasUsed * gasPrice`; if no owner is registered nothing is credited. Special transactions (to 0x89 and 0x90) pay and receive no fee.

**Source**
- `core/state_transition.go:881-930` fee credit; `:421-433` the `owner` argument.
- `core/state_processor.go:96-102` `getCoinbaseOwner`; `core/state/statedb_utils.go:114-121` `GetCandidateOwner`; `core/state/state_reader.go:18-24` the mapping slot.
- `core/types/transaction.go:740-742` `IsSpecialTx`.

**Networks.** All; the block is `effective.tip_trc21_fee`.

**Rust.** `XdcRules::fee_to_owner`, `XdcChain::fee_owner`, `XdcHandler::reward_beneficiary`. The owner is an input; deriving it from state, special transactions and TRC21 token fees belong to XB-08. The sweep fills the owner with `eth_getStorageAt` and excludes special and TRC21 token-fee transactions with a listed reason.
