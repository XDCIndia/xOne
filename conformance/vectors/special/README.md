# Special-transaction vectors (XB-08)

Apothem (chain 51) archive answers, written by `xone-sweep special <name>` (`conformance/sweep/src/special.rs`). Never edit a vector by hand: run the exporter again. Read by `conformance/sweep/tests/special_vectors.rs`, `conformance/sweep/tests/special.rs` and `crates/xone-state/tests/system_contracts.rs`. Oracle: geth-XDC at `conformance/PIN`; the archive is `xdc02:21105` (read-only, at most 4 requests per second), export date 2026-11-11.

| file | what | exporter |
| --- | --- | --- |
| `sign_blocks.jsonl` | whole blocks with sign transactions after `TIPSigning`: every transaction, its archive receipt, the block's receipts root, the nonces before and after of sign-only senders. A block is kept only if the archive receipts, encoded by `Receipt::encode`, hash to the header's receipts root | `special sign 60 1` |
| `tip_signing_apothem.json` | all accounts of the state after block 2,999,999 (`debug_accountRange`; each account as the leaf of its `eth_getProof`, because the range API cuts a wide balance) and the two state roots | `special tip-signing` |
| `fee_recipient.jsonl` | blocks with fee-paying transactions: author, registered owner, base fee, gas and effective price of each transaction, balances of author and owner before and after | `special fees 15 1` |
| `base_fee.jsonl` | `baseFeePerGas` of the headers around `Eip1559Block` and `Gas2500xBlock` and of random blocks of each tier | `special base-fee` |
| `base_fee_negative.jsonl` | wrong-tier base-fee headers made from the archive headers (XA-04's line shape without the block body; not in `consensus/negative/`, see `spec/evm/base-fee-tiers.md`) | `special base-fee` |
| `system_contracts.jsonl` | `eth_call` answers of 0x88, 0x89, 0x90 and the storage each call read | `special slots 100 1` |
| `cases/*.json` | single transactions for the sweep's special mode: `sign-evm-*` (to 0x89 before `TIPSigning`), `sign-v1-before-trc21-*`, `sign-v1-after-trc21-*`, `sign-v2-*`, `randomize-*`, `xdcx-empty-*`, `oog-0x88-*` | `special cases 1`, `special oog` |

`TIPSigning` on Apothem is block 3,000,000 (`effective.tip_signing`), not 23,779,191; see `spec/evm/special-transactions.md`.

`oog-0x88-*` is the archive's `debug_traceCall` of a call to 0x88 with too little gas, not a mined transaction.

Run `xone-sweep run special` to replay the cases against the router.
