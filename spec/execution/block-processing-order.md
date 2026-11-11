# Block processing order

Crate: `xone-executor` (`execute_block`). Reference: geth-XDC at `conformance/PIN` (`6fc8e8c2088b7f18264f5663036669d04fa7becf`). Gate: `conformance/harness` (`xone-replay run`), see `conformance/harness/RUNS.md`.

A block is executed on the state of its parent, in this order. The result holds the state diff, receipts, gas used, bloom and the transactions root, receipts root and state root, all computed from the body and the executed state. Nothing is copied from the header; the caller compares.

| # | Step | geth-XDC (file:line) | xOne |
| --- | --- | --- | --- |
| 0 | Header verification, then base fee check, before any state is read | `consensus/XDPoS/xdpos.go:547` `VerifyHeader`; `consensus/misc/eip1559/eip1559.go:63-99` | `HeaderVerifier::verify`, `xone_evm::basefee::verify` |
| 1 | Copy of the parent state for the reward (`parentState`): owners and voters are read from it | `core/state_processor.go:123`, `:276-277` | `parent_view` is read-only; `Holders::read` reads it |
| 2 | `TIPSigning` wipe of 0x89 at the fork block, before the first transaction | `core/state_processor.go:152`, `:321-327` | `xone_evm::special::tip_signing_wipe` |
| 3 | EIP-4788 (not on XDC), Multicall3 and EIP-2935 at Prague | `core/state_processor.go:336-350` | not reached: `rules()` refuses Prague and later (`xone_evm::spec`) |
| 4 | Fee owner of the author, read once from the validator contract 0x88 in the state before the loop; author is the seal signer | `core/state_processor.go:103`, `:182`; `core/evm.go:43-52` | `fees::block_author`, `validator_0x88::candidate_owner` |
| 5 | TRC21 token fee capacity | `core/state_processor.go:189`, `:254`, `:268` | not supported when it applies: a block at or above `TIPTRC21Fee` whose issuer (`0x8c0f...`, Apothem `0x0E2C...`) lists a token (slot 1 not zero) is `ExecError::Unsupported`; with no token listed the capacity map is empty and the step does nothing |
| 6 | Per transaction: recover the sender, then route | `core/state_processor.go:210`, `:235-258`, `:856-872` | `tx::decode`, `xone_evm::route::route` |
| 6a | Sign transaction (0x89 from `TIPSigning`): no EVM, no gas, nonce + 1, one log `{0x89}`, legacy receipt, cumulative gas unchanged | `core/state_processor.go:883-933` | `special::apply_sign_tx` |
| 6b | XDCx empty transaction (0x91-0x94 in the receiver window): no state change, one log `{to}` | `core/state_processor.go:943-975` | `special::apply_empty_tx` |
| 6c | Any other transaction (including 0x89 before `TIPSigning`, and 0x90): the pool must hold the whole gas limit, then the EVM, then `Finalise(IsEIP158)`; receipt gas is `gasUsed` after refund | `core/state_processor.go:437-475`; `core/state_transition.go:421`, `:497`, `:684`, `:881-930` | `XdcHandler::run`, `Overlay::commit` |
| 7 | Receipts: bloom of each, block bloom is their union | `core/state_processor.go:479-520` | `Receipt::new`, fold in `execute_block` |
| 8 | Withdrawals and Prague system calls | `core/state_processor.go:269`, `:357-400` | none on XDC (no withdrawals in a V1 or V2 body) |
| 9 | V1 checkpoint reward, when `number % RewardCheckpoint == 0` and `number > RewardCheckpoint`; window `n-2r+1 ..= n-r`, masternodes of header `n-2r`, signs counted along the parent chain, paid with `AddBalance` from the parent state | `consensus/XDPoS/xdpos.go:1573-1612`; `eth/hooks/engine_v2_hooks.go:81`, `:262-295`; `consensus/XDPoS/reward.go:71-215` | `reward::v1::{locate, rewards}`, `SignTxCounter`, `Overlay::credit` |
| 10 | State root after the reward: `IntermediateRoot(IsEIP158)` | `consensus/XDPoS/xdpos.go:1615` | `StateView::root_with` over `xone_state::trie` |
| 11 | After the block is written, at a V1 gap block (`n % epoch == epoch - gap`) the snapshot signers become the cap-sorted candidates of 0x88 in that block's state | `core/blockchain.go:2319`, `:3963-3990`; `consensus/XDPoS/xdpos.go:2135-2211` | runner (`conformance/harness`), `checkpoint::select_candidates` |

## Points that matter

- The reward reads `parentState`, the state before the transactions of the checkpoint block, not the state after them (`state_processor.go:123`). A checkpoint block with transactions pays the same reward either way only by accident.
- The first checkpoint (`n = RewardCheckpoint`) pays nothing: the hook is gated by `number > rCheckpoint` (`engine_v2_hooks.go:81`). `reward::v1::locate` would underflow there, so the executor applies the gate first.
- Sign transactions before `TIPSigning` run in the EVM against the 0x89 contract and count in the reward only with a successful receipt (`reward.go:144`); from `TIPSigning` they count by presence (`reward.go:134`). The count reads the runner's own receipts, not the archive's.
- An account deleted by SELFDESTRUCT or by EIP-161 takes its storage with it; recreating it in the same block is `OverlayError::Recreated` (not covered).
- An error of the reward hook is only logged by geth (`xdpos.go:1595-1599`); the executor treats it as an error of the block, which is stricter.

## Not covered by the executor

TRC21 token fees, V2 and tiered rewards, the V2 masternode snapshot at gap blocks, blacklist checks (`state_processor.go:223-229`, a rejection that a canonical block never needs), wide balances above 2^256 between transactions (`OverlayError::Wide`), Prague and later.
