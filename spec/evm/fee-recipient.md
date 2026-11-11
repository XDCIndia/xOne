# The transaction fee goes to the coinbase, or after `TIPTRC21Fee` to the coinbase owner

**Rule.** Before `TIPTRC21Fee` (strictly greater than the block) the coinbase receives `gasUsed * (gasPrice - baseFee)`; the base fee is subtracted only when the header has one (from `Eip1559Block`). After it the owner of the masternode (`validatorsState[coinbase].owner` in the validator contract 0x88, read from the state at the start of the block) receives `gasUsed * gasPrice`; if no owner is registered nothing is credited. Special transactions (to 0x89 and 0x90) pay and receive no fee.

**Source**
- `core/state_transition.go:881-930` fee credit; `:421-433` the `owner` argument.
- `core/state_processor.go:96-102` `getCoinbaseOwner`; `core/state/statedb_utils.go:114-121` `GetCandidateOwner`; `core/state/state_reader.go:18-24` the mapping slot.
- `core/types/transaction.go:740-742` `IsSpecialTx`.

**Networks.** All; the block is `effective.tip_trc21_fee`.

**Who is the author.** The coinbase of the EVM is the block's creator (`engine.Author`): the recovered signer of the seal for V1 (the header's coinbase is zero), the recovered signer of the `validator` signature for V2. It is never `header.Coinbase` on V1 (xOneGo 39ea9b05, 7278ae91).

**No burn.** After `TIPTRC21Fee` the owner receives `gasUsed * gasPrice` in full; the base fee is not subtracted (`core/state_transition.go:900-912`, xOneGo#672). Before it the base fee is subtracted when the header has one, and that part goes to no one. With no registered owner nothing is credited; the fee does not fall back to the author (`:909`). A special transaction credits nothing at any block (`:881`).

**Boundary.** The comparison is strict: block `TIPTRC21Fee` itself still pays the author, the next block pays the owner (xOneGo#615). Apothem 23,779,191, mainnet 38,383,838.

**Rust.** `xone_evm::fees::{credit, block_author}` (the handler's `reward_beneficiary` calls `credit`), `XdcRules::fee_to_owner`, `XdcChain::fee_owner`. The owner is an input: the caller reads it with `xone_state::system_contracts::validator_0x88::candidate_owner(state, author)` on the state at the start of the block (`xone-evm` does not depend on `xone-state`).

**Vectors.** `conformance/vectors/special/fee_recipient.jsonl`: blocks with fee-paying transactions, the receipts' gas and effective price, and the balances of the author and of the owner before and after the block from the archive. The test sums `credit` over the transactions and compares with each balance change. Not used: blocks where the author or owner is a party to a transaction, checkpoints and V2 epoch switches (rewards). `conformance/sweep/tests/special_vectors.rs`.

**Not covered.** TRC21 token-fee transactions (the sender pays no gas; the token's fee capacity pays): no vector was found, see `special-transactions.md`. Mainnet (chain 50): the rows are checked, no archive balances.
