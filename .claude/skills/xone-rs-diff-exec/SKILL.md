---
name: xone-rs-diff-exec
description: Per-block differential execution of the xOne Rust client against the xdc02 geth-XDC archive. Compares hash, stateRoot, receiptsRoot, gasUsed, logsBloom and extraData for every block, and on a mismatch narrows it to the first differing transaction, account and storage slot. Use when the task says diff-exec, differential, parity run, "compare with geth", "wrong state root", "invalid merkle root", "bad block", divergence, bisect a mismatch, or when a milestone range must be proven at 0 mismatches.
---

# xone-rs-diff-exec

This is the method that found the reth `SwitchEpoch=0` bug: the receipts matched, the state root did not, and only an account-level diff showed the wrong MintedRecord key.

## Operating rules

1. The archive is read-only. Never send a transaction, never call a `miner_`, `admin_` or `personal_` method on it.
2. Compare all six header fields. Receipts root and gas alone hide state-only bugs.
3. The first mismatch stops the run. Do not continue past it and do not count later blocks.
4. A mismatch on a non-head side block is recorded, not ignored.
5. Rate-limit archive calls so that the xdc02 node keeps following its chain.
6. The output of a run is evidence only when it states range, mismatches, client commit, geth pin and host.

## Inputs

- Network (`apothem`, `mainnet`, `net2040`) and block range `from..to`.
- The xOne binary or the offline executor, with its stamped version.
- Archive RPC URL (xdc02). For offline runs: the exported vectors directory.

## Steps

1. **Pre-flight.** Check the archive is at or past `to`. Check disk on the run host is below 85%. Record the client stamp and the geth pin.
2. **Header pass.** For each block: `hash`, `stateRoot`, `receiptsRoot`, `gasUsed`, `logsBloom`, `extraData`, plus `transactionsRoot` against the body. Stop at the first difference.
3. **Receipt pass (on mismatch).** Compare every receipt of the block: status, cumulative gas, logs. The first differing receipt names the transaction.
4. **State pass (on mismatch).** Get the block's prestate and post-state diff from the archive (`debug_traceBlockByNumber` with `prestateTracer`, `diffMode: true`). Compare with the client's write set. Report the first differing account, then field (nonce, balance including the wide part, code hash), then storage slot.
5. **Trace pass (if the transaction is known).** Compare a struct-log or call trace of that transaction step by step. The first differing opcode, gas value or stack item is the lead.
6. **Consensus pass (at a checkpoint or epoch switch).** Compare reward per owner, `Validators`, `Penalties`, and the masternode order with the sidecar oracle. A state-only mismatch at `N % 900 == 0` is almost always here.
7. **Divergence record.** Write `conformance/divergences/<network>-<block>.md`: block, field, first differing item, both values, the rule suspected, the geth `file:line`.
8. **Hand over.** Open or update the work item and run `xone-rs-conformance-loop` from step 9 (regression vector first).

## Reading the result

| Symptom | Look first at |
|---|---|
| gasUsed differs | gas table, pre-EIP-2028 calldata price, special-tx gas, precompile set |
| receipts differ, gas equal | log order, status of a special tx, return-data handling |
| only stateRoot differs | rewards, penalties, MintedRecord keys, wide balance, nonce of a sign-tx sender |
| only at the first checkpoint after a snap start | bodies below the pivot, gap-block candidate snapshot |
| only after a restart or a sibling block | state of an uncommitted side block, canonical index |
| hash differs, roots equal | header field encoding, `Validators` or `Penalties` bytes, extra data |

## Gates

- 0 mismatches over the whole requested range, or one divergence record for the first mismatch.
- The run log states range, count, commit, pin and host.

## Done

Either "range X..Y, N blocks, 0 mismatches, commit C, pin P, host H" is posted on the work item, or the divergence record exists and the loop has the next step.
