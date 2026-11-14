# ADR-009: XDCx DEX and lending scope (empty-transaction routing only, as geth-XDC)

* Status: **Proposed -> Accepted (proposal).** The text below is the proposal the owner signs. It stays Proposed until the record in "Owner acceptance record" is signed. Nobody but the owner changes this line. Issue #23 (XB-03).
* Date: 2026-11-14
* Pins: xOne `origin/main` `a99fcf7290fc9a78be30a700badb7504a931be2a`; geth-XDC oracle `XDCIndia/go-ethereum` `6fc8e8c2088b7f18264f5663036669d04fa7becf` (`conformance/PIN`). Every geth line below was read from a tarball of that SHA on 2026-11-14.
* Related: ADR-008 working scope `spec/rpc/scope-adr-008.md` (no XDCx namespaces); `spec/evm/special-transactions.md` (the routing rule); issue #10 (XA-05 sequential replay).

## Decision

**xOne does not implement the XDCx DEX engine or the XDCx lending engine.** It does what geth-XDC does at the pin:

1. A transaction whose `to` is `0x91`, `0x92`, `0x93` or `0x94` is an **XDCx empty transaction** while `IsTIPXDCXReceiver(block)` holds. It has no EVM run, no gas, no nonce change and no state change. Its receipt is a successful legacy receipt with `GasUsed = 0` and one log whose address is the target and whose topics and data are empty.
2. Outside that window the same transaction is an ordinary EVM transaction to an address with no code.
3. xOne serves no `XDCx*` or `XDCxlending*` RPC method. A call returns JSON-RPC `-32601`.
4. The XDCx precompile sets that exist in geth-XDC rules stay as they are (they are EVM rules, not the engine; see `spec/evm/modexp-and-precompile-sets.md`). This ADR does not change them.

Why: the plan checklist asks for an explicit in or out decision. A silent omission would look like a bug later, and a partial port would add a large, untested state machine. geth-XDC, the parity oracle, does not run the engine either (see Evidence).

## Evidence

All `file:line` at geth-XDC `6fc8e8c2`.

**Routing (verified by reading the file).**

* [`core/state_processor.go:856-872`](https://github.com/XDCIndia/go-ethereum/blob/6fc8e8c2088b7f18264f5663036669d04fa7becf/core/state_processor.go#L856-L872) `RouteTransaction`: `to` equal to `TradingStateAddrBinary`, `XDCXLendingAddressBinary`, `XDCXAddrBinary` or `XDCXLendingFinalizedTradeAddressBinary` returns `TxRouteEmpty` when `config.IsTIPXDCXReceiver(blockNumber)`; otherwise `TxRouteEVM`.
* [`core/state_processor.go:943-975`](https://github.com/XDCIndia/go-ethereum/blob/6fc8e8c2088b7f18264f5663036669d04fa7becf/core/state_processor.go#L943-L975) `ApplyEmptyTransaction`: finalise (`Byzantium`) or intermediate root (before), one empty log with the target address, a legacy receipt with status 1, `GasUsed = 0`.
* [`params/config.go:3477-3480`](https://github.com/XDCIndia/go-ethereum/blob/6fc8e8c2088b7f18264f5663036669d04fa7becf/params/config.go#L3477-L3480) `IsTIPXDCXReceiver`: forked at `TIPXDCX` and **not** forked at `TIPXDCXReceiverDisable`. It is a window, not a one-way switch.
* [`common/xdc_contracts.go:36-48`](https://github.com/XDCIndia/go-ethereum/blob/6fc8e8c2088b7f18264f5663036669d04fa7becf/common/xdc_contracts.go#L36-L48) the four addresses: `0x91` XDCx, `0x92` trading state, `0x93` lending, `0x94` finalized lending trade. The issue text cites `common/types.go:51-54` for these; at the pin they are in `common/xdc_contracts.go`.
* [`common/types.go:100-110`](https://github.com/XDCIndia/go-ethereum/blob/6fc8e8c2088b7f18264f5663036669d04fa7becf/common/types.go#L100-L110) the mainnet `TIPXDCX*` constants; per-chain values by `CopyXDCConstants` (see `spec/chainspec/non-parameter-switches.md`, A4 and A5).

**The engine is not run (verified by `grep` over the tarball).**

* The `XDCx/` directory has one file, `XDCx/XDCx.go`, 28 lines. Its header says "Decentralized Exchange Stub for geth 1.17 compatibility"; `ProcessOrder` returns `nil` and `GetOrderBook` returns `nil`.
* No call of `XDCx.New`, `XDCxlending.New` or `xdcx.New` exists under `eth/`, `node/`, `cmd/`, `core/` or `miner/` at the pin.
* geth-XDC is the oracle the other clients matched on Apothem and mainnet (tracker go-ethereum#1950, reported, not re-run here).
* The erigon cross-check of the CertiK report marked XDC-01 and XDC-02 (XDCx order and lending, trading-state halt) not applicable because the subsystem was never ported (XDCIndia/xOneGo#407, reported, not re-run here).

**Windows per network (from `crates/xone-chainspec/generated/forks.json`, keys `effective.tipXDCX` and `effective.tipXDCXReceiverDisable`).** The window is `[tipXDCX, tipXDCXReceiverDisable)`.

| Network | Chain id | First empty-routed block | First block routed to the EVM again |
| --- | --- | --- | --- |
| mainnet | 50 | 38,383,838 | 80,370,900 |
| apothem | 51 | 23,779,191 | 66,825,000 |
| devnet | 5551 | 0 | 100,000,000,000,000,000 (effectively never) |
| devnet551 | 551 | 0 | 0 (the window is empty) |
| net2040 | 2040 | 7,200 | 11,700 |

**What is merged on `main` and checks this (verified by reading it, not re-run here).**

| Piece | Where |
| --- | --- |
| Route table: `route()` returns `XdcxEmpty` for the four addresses when `rules.xdcx_empty_route` | `crates/xone-evm/src/route.rs` (`XDCX`, `TRADING_STATE`, `XDCX_LENDING`, `XDCX_LENDING_FINALIZED_TRADE`); the switch is `spec.is_tip_xdcx_receiver(n)` at `crates/xone-evm/src/spec.rs:119` |
| Receipt: `apply_empty_tx` (no state change, one log with the target, `PreByzantium` is an error) | `crates/xone-evm/src/special.rs:126-143` |
| Executor: `Route::XdcxEmpty` builds that receipt | `crates/xone-executor/src/lib.rs:354-360` |
| Window edges per network | test `xdcx_empty_route_is_the_receiver_window_and_randomize_is_always_evm`, `conformance/sweep/tests/special_vectors.rs:648` |
| Archive vectors: 8 Apothem blocks (24,678,565 to 55,111,473) with the archive receipt (status 1, gas 0, one log) | `conformance/vectors/special/cases/xdcx-empty-*.json`; class test with the route switched off (a mutant) `conformance/sweep/tests/special.rs:66` |
| RPC: `XDCx_getOrderBook` and `XDCxlending_x` answer `-32601`; no namespace outside the ADR-008 list | `crates/xone-rpc/tests/rpc.rs:319-345` |

## Not verified, and gaps against the issue's acceptance list

* **Inference (not verified).** Canonical XDPoSChain history before `TIPXDCXReceiver` may contain XDCx transactions whose effect on the main state root is nil. The sequential replay of issue #10 (XA-05) confirms or refutes it.
* **Vectors per address.** The 8 archive vectors all target `0x92`. There is no vector for `0x91`, `0x93` or `0x94`, and none for the block just before `TIPXDCX` or at `TIPXDCXReceiverDisable` (the window edges are tested by the table test above, not by oracle receipts). Issue #23 asks for one vector per address before and after `TIPXDCXReceiver` (receipt, gas, state root).
* **Amsterdam.** `crates/xone-executor/src/lib.rs:349` refuses an XDCx empty transaction in an Amsterdam block (`Unsupported`). geth routes it the same way at any height; the refusal is xOne-only and has no evidence for or against it. No built-in network has an XDCx window and an Amsterdam block at the same height (net2040: window ends 11,700, Amsterdam 21,600).
* **Spec page.** `spec/evm/special-transactions.md` already holds the routing rule. The window values are not copied into `spec/` (README rule: pages do not carry table values); they are in `generated/forks.json` and in the table above.
* **Mainnet.** No mainnet archive was available, so the mainnet window is the exported constant, not an observed receipt.

## Re-open trigger

Re-open this ADR, with a written choice to the owner, if any of these holds:

1. Any network enables XDCx state (a client that runs the engine on that network, or a chain spec whose `TIPXDCXReceiverDisable` is unset while the engine is expected).
2. The sequential replay (#10) shows a mismatch at a transaction to `0x91` to `0x94`.
3. geth-XDC starts calling `XDCx.New` or `XDCxlending.New`, or `XDCx/` grows beyond the stub, when the pin moves.
4. The owner requires XDCx RPC methods or an order book (this would add a multi-month track and a new state trie).

## Consequences

* `ChainSpec` and the router keep the window as data; no flag disables it.
* The RPC scope lists the XDCx namespaces as absent (`spec/rpc/scope-adr-008.md`).
* Follow-up work, not done by this ADR: vectors for `0x91`, `0x93`, `0x94` and the window edges, exported with the conformance exporter (needs the archive host, which this lane does not use).

## Owner acceptance record

To accept, the owner replaces the Status line with `Status: **Accepted**, <date>, <owner>` and fills this block.

| Item | Proposed value | Owner entry |
| --- | --- | --- |
| Scope | empty-transaction routing only; no XDCx engine, no XDCx RPC | accept / require a full port: ______ |
| Amsterdam refusal in the executor | keep (xOne-only, no evidence) | keep / route as empty: ______ |
| Missing vectors (`0x91`, `0x93`, `0x94`, window edges) | file a follow-up for the archive export | yes / no: ______ |
| Signed | | name, date: ______ |
