# STATICCALL touches the callee only from the Istanbul rules

**Rule.** `StaticCall` adds a zero balance to the callee, which touches it, so EIP-161 deletes it at the end of the transaction if it is empty. On XDC this is done only when `IsIstanbul` holds. Before, an empty callee such as precompile 0x01 keeps its trie leaf; touching it moves the state root by one leaf.

**Source**
- `core/vm/evm.go:513-515` the gate; the comment cites mainnet block 34,059,322.

**Networks.** All; the switch block is `effective.istanbul`.

**Rust.** `XdcRules::static_call_touch`. `XdcHandler::run_exec_loop` turns the zero-value transfer of a STATICCALL frame into an apparent value, which skips the touch. The archive cannot show this rule: `prestateTracer` drops empty accounts. The oracle (`conformance/vectors/evm/oracle/touch.go`) runs a contract that STATICCALLs an empty account at the blocks around the switch; `tests/static_call_touch.rs` compares.
