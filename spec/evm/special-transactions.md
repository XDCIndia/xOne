# Sign, randomize and XDCx transactions bypass or skip parts of the EVM, and 0x89 is wiped at `TIPSigning`

**Rule (routing).** Only the target and the block number choose the handler (`RouteTransaction`, `core/state_processor.go:856-872`, checks in this order):

1. no target (a creation): EVM;
2. target 0x89 and `IsTIPSigning(block)` (`>=`, `params/config.go:3299`): a **sign transaction**, no EVM;
3. target 0x91, 0x92, 0x93 or 0x94 (`common/xdc_contracts.go:38-48`) and `IsTIPXDCXReceiver(block)` (from `TIPXDCX`, up to but excluding `TIPXDCXReceiverDisable`, `params/config.go:3477-3480`): an **XDCx empty transaction**, no EVM (ADR-009 working assumption, issue 23: xOne routes them as geth-XDC does and runs no XDCx engine);
4. everything else, a call to 0x90 included: EVM.

There is **no check of the sender, the gas price or the calldata** in this decision, nor in `Process` (`core/state_processor.go:230-262`). "From a masternode, gas price zero, exactly 68 bytes of `sign(uint256,bytes32)`" is txpool admission (`core/txpool/legacypool/legacypool.go`, `hasKnownSpecialCalldata`, XB-16) and the reward count (`consensus/XDPoS/reward.go:56`, XB-10). A block that carries a sign transaction with other calldata is applied the same way.

**Special transaction.** `IsSpecialTx` is a target of 0x89 or 0x90 at any block (`core/types/transaction.go:740-742`). It is exempt from the fee-cap floor (`core/state_transition.go:684`) and credits no fee (`:881`). That includes a call to 0x89 before `TIPSigning`, which runs in the EVM.

**Sign transaction** (`ApplySigningTransaction`, `core/state_processor.go:883-933`): the sender's nonce must equal the transaction's (else the block is invalid, `:894-897`); the nonce goes up by one; no gas is used and the block's running gas does not change; one log with address 0x89, **no topics, no data** (the `Sign` event never fires, `:899-905`); the receipt is a legacy receipt whatever the transaction type, status 1, `gasUsed` 0, `cumulativeGasUsed` the running total (`:917-927`); its bloom has only the address of 0x89. Before Byzantium geth puts an intermediate root in the receipt; every network has `TIPSigning` after Byzantium, so xOne returns a typed error instead.

**XDCx empty transaction** (`ApplyEmptyTransaction`, `:943-975`): no state change, not even the nonce; one log with the target as address; same receipt shape.

**`TIPSigning` block** (`ApplyTIPSigningHardFork`, `core/state_processor.go:321-327`, called at `:152` before the first transaction and before the pre-execution calls): at the block equal to `EffectiveTIPSigningBlock` (`params/config.go:3307`, `Cmp == 0`) geth runs `IntermediateRoot(false)`, `SelfDestruct(0x89)`, `Finalise(true)`, `IntermediateRoot(true)`. The first root is discarded (it opens the trie for the path database). The change is the removal of account 0x89 with its code and storage; an absent 0x89 is a no-op. Transactions of that same block to 0x89 are already sign transactions (`>=`).

**Networks.** `effective.tip_signing` is 3,000,000 on mainnet **and on Apothem** (`common/types.go:98`, `:215`, `:231`), 0 on devnet 551. The value 23,779,191 that issue 28 and its brief give for Apothem is `TIPTRC21Fee`, `TIPXDCX` and `TIPXDCXCancellationFee` there. The archive agrees: 0x89 has code and 1 storage root at block 2,999,999 and does not exist at 3,000,000 (`eth_getProof`). XDCx window: Apothem 23,779,191 to 66,825,000, mainnet 38,383,838 to 80,370,900, devnet 551 empty (`common/types.go:210-260`). The window end is `effective.tip_xdcx_receiver_disable`, added to the exporter for this item.

**Rust.** `xone_evm::route::{route, is_special_tx}` (switches `XdcRules::sign_tx_route`, `xdcx_empty_route`), `xone_evm::special::{apply_sign_tx, apply_empty_tx, tip_signing_wipe, Receipt}`, handler `validate_env` for the floor exemption. `xone-evm` takes plain values (the sender's nonce, the block number) and returns the nonce and the address to remove; it does not depend on `xone-state` (`ci/layering.py`), so the caller reads and writes the state.

**Vectors.** `conformance/vectors/special/sign_blocks.jsonl` (whole blocks, archive receipts, receipts root and sign-only sender nonces), `tip_signing_apothem.json` (all 351 accounts of block 2,999,999, each as the leaf of its account proof and both state roots), `cases/*.json` (single transactions of the sweep's special mode). Tests `conformance/sweep/tests/special_vectors.rs` (they need `xone-state` for the trie), `conformance/sweep/tests/special.rs`, `crates/xone-evm/tests/special_fee_floor.rs`.

**Not covered.** TRC21 token-fee transactions (`ApplyTRC21TokenFee`, `SettleTRC21TokenFee`, `UpdateTRC21Fee`, `core/state_processor.go:254-268`, `:977-1020`, `core/state/trc21_reader.go`): no such transaction is in the sample, so no vector. Mainnet: no archive access, so the mainnet `TIPSigning` root is not checked. The XDCx empty receipt is checked against the archive only where the sampler found such transactions (see the report).
