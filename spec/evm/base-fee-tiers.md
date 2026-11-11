# The base fee is a fixed tier chosen by the block's own number, absent before `Eip1559Block`

**Rule.** XDC has the EIP-1559 header field and the dynamic-fee transaction types but no fee market.

- Before `Eip1559Block` (`IsEIP1559`, `params/config.go:3046`, the row alone) the header has no base fee, and a header that has one is invalid: `invalid baseFee: have <n>, want <nil>` (`consensus/misc/eip1559/eip1559.go:63-69`). From it, a header without one is invalid: `header is missing baseFee` (`:84`).
- From it the base fee is `XDCBaseFee` = 12.5 gwei (`params/protocol_params.go:175`), and 50 times that, 625 gwei, from the `Gas2500xBlock` row (`params/xdc_gas.go:31-42`, `params/config.go:3418`; a `nil` row never activates).
- The tier is that of the **block's own number**; the child of block `p` carries the tier of `p + 1` (`CalcBaseFee`, `eip1559.go:121-127`). So the Gas2500x block itself already carries 625 gwei, not the previous tier. The check is an exact comparison without the parent (`:95-98`: `invalid baseFee: have %s, want %s`). The gas-limit check of the same function is the header verifier's.
- Nothing depends on gas use. The fee is **credited, not burned** (`fee-recipient.md`).
- Special transactions (to 0x89 or 0x90) are exempt from the fee-cap floor in the state transition (`core/state_transition.go:684`); the pool floor (`core/txpool/validation.go:409-420`) is XB-16.
- The BASEFEE opcode in the London to `Eip1559Block` window, where the header has none, reports 12.5 gwei (`params/xdc_gas.go:44-49`, `spec.rs`); that is a different rule from the header field.

**Networks.** Each network's own rows: Apothem `Eip1559Block` 71,550,000 and `Gas2500xBlock` 83,600,000; mainnet `Eip1559Block` 98,800,200 and no `Gas2500xBlock`; devnet 551 25,000 and 1,385,100. Do not copy a height between networks.

**Rust.** `xone_evm::basefee::{base_fee_for_block, child_base_fee, verify, BaseFeeError::{Unexpected, Missing, Wrong}}`.

**Vectors.** `conformance/vectors/special/base_fee.jsonl`: the archive headers' `baseFeePerGas` at Eip1559Block and Gas2500xBlock minus 10,000, minus 2, minus 1, at, plus 1, plus 2, plus 10,000, and random blocks of each tier. `conformance/vectors/special/base_fee_negative.jsonl`: wrong headers made from them (previous tier at the Gas2500x block and after it, the 625 gwei tier too early, off by one wei, a base fee before the fork, a missing base fee). The line shape is XA-04's header part (`conformance/vectors/consensus/README.md`: `id`, `network`, `class`, `number`, `hash`, `header`, `parent`, `mutation`) without `body`, `receipts`, `context` and `oracle`. The file is **not** in `conformance/vectors/consensus/negative/` because `crates/xone-consensus/tests/corpus.rs` loads every file there as a whole-block vector and expects the `HeaderError` variants of the XA-04 verifier; `mutation.expect` here names the variant of `BaseFeeError`. XA-04's `v2_base_fee_wrong_tier` and `v2_base_fee_missing` cover random V2 blocks; these cover the two boundaries and a base fee before the fork. Tests `conformance/sweep/tests/special_vectors.rs`.

**Not covered.** Mainnet and net2040 have no archive vector (no access); the function is checked there only against the fork rows.
