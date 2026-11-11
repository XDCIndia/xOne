---
name: xone-rs-diff-exec
description: Sequential differential execution of the xOne Rust client against the xdc02 geth-XDC archive. Primary mode replays blocks on the candidate's OWN accumulated state, from genesis or an authenticated pivot, and compares independently computed roots and consensus auxiliaries. Oracle prestate replay is diagnostic only. Use when the task says diff-exec, differential, parity run, "compare with geth", "wrong state root", "invalid merkle root", "bad block", divergence, bisect a mismatch, or when a gate range must be proven at 0 mismatches.
---

# xone-rs-diff-exec

This method found the reth `SwitchEpoch=0` bug: receipts matched, the state root did not, and only an account-level diff showed the wrong MintedRecord key. It also carries a lesson from live xOne work: a replay on oracle prestate showed the EVM was correct at the 85,214,323 wedge, while the fault was side-block and fork resolution on accumulated state. A per-block fixture on fresh oracle prestate cannot see that class. Hence the mode order below.

## Modes

| Mode | State the candidate executes on | What it can prove | Evidence status |
|---|---|---|---|
| **Sequential (primary)** | Its own state, built from genesis or from an authenticated pivot, block after block, including restarts and unwinds in the schedule | Accumulated-state, fork-resolution, restart and unwind correctness | Counts toward gates |
| **Oracle prestate (diagnostic)** | The archive's prestate for one block, loaded fresh | Whether one block's EVM and consensus code is right in isolation | Diagnostic only. Never counts toward a gate. |

A passing oracle-prestate run does not close a gate. A failing sequential run is narrowed with the oracle-prestate mode, then fixed, then re-run sequentially from the same start.

## Operating rules

1. The archive is read-only. Never send a transaction, never call a `miner_`, `admin_` or `personal_` method on it.
2. **Start point.** Genesis, or an authenticated pivot (`xone-rs-conformance-loop` names the binding: chain ID, genesis, checkpoint hash, state root, certificate provenance). A pivot taken from the archive without that check is not authenticated.
3. **Independent computation.** Recompute `transactionsRoot`, `receiptsRoot`, `stateRoot`, `logsBloom`, the header hash and the extra-data fields from the candidate's own execution and encoders. Comparing the candidate's header with the imported header it was given is tautological and is not a check. Verify signatures, `Validators`, `Penalties` and the reward result by computation, not by equality with the input.
4. Compare all six header fields plus consensus auxiliaries: reward per owner, `Validators`, `Penalties`, masternode order, MintedRecord keys, canonical index.
5. The first mismatch stops the run. Do not continue past it and do not count later blocks.
6. A mismatch on a non-head side block is recorded, not ignored.
7. Rate-limit archive calls so that the xdc02 node keeps following its chain.
8. A run is evidence only when it states mode, start point, range, mismatches, client commit, geth pin and host.
9. For a produced block (not a followed one): compare under fixed inputs, or check rule validity. Valid timestamps and transaction choices can give a different, still valid, block hash. Do not call a different hash a failure without checking validity.

## Inputs

- Network (`apothem`, `mainnet`, `net2040`), start point, and block range `from..to`.
- The xOne binary or the offline executor, with its stamped version.
- Archive RPC URL (xdc02). For offline runs: the exported vectors directory.
- Optional fault schedule: restarts, unwinds and sibling imports at listed blocks (the run replays on the same state through them).

## Steps

1. **Pre-flight.** Check the archive is at or past `to`. Check disk on the run host is below 85%. Record the client stamp and the geth pin.
2. **Sequential pass.** For each block from the start point: import on candidate-owned state; recompute the commitments (rule 3); compare with the archive. Stop at the first difference.
3. **Receipt pass (on mismatch).** Compare every receipt of the block: status, cumulative gas, logs. The first differing receipt names the transaction.
4. **Isolation pass (on mismatch, diagnostic).** Get the block's prestate and post-state diff from the archive (`debug_traceBlockByNumber` with `prestateTracer`, `diffMode: true`). Execute the block on that prestate. If it matches, the fault is in accumulated state, side-block handling or restart, not in the block logic: go to step 6. If not, compare the write set: first differing account, then field (nonce, balance including the wide part, code hash), then storage slot.
5. **Trace pass (if the transaction is known).** Compare a struct-log or call trace step by step. The first differing opcode, gas value or stack item is the lead.
6. **Accumulated-state pass.** Compare the candidate's state at block `N-1` with the archive's post-state of `N-1` for the accounts the block touched. Check the canonical index, side-block lineage and consensus snapshots. A difference here that the isolation pass did not show is the finding.
7. **Consensus pass (at a checkpoint or epoch switch).** Compare reward per owner, `Validators`, `Penalties`, and the masternode order with the sidecar oracle. A state-only mismatch at `N % 900 == 0` is almost always here.
8. **Divergence record.** Write `conformance/divergences/<network>-<block>.md`: mode, block, field, first differing item, both values, the rule suspected, the geth `file:line`.
9. **Hand over.** Open or update the work item and run `xone-rs-conformance-loop` from its regression-vector step.

## Reading the result

| Symptom | Look first at |
|---|---|
| gasUsed differs | gas table, pre-EIP-2028 calldata price, special-tx gas, precompile set |
| receipts differ, gas equal | log order, status of a special tx, return-data handling |
| only stateRoot differs | rewards, penalties, MintedRecord keys, wide balance, nonce of a sign-tx sender |
| sequential fails, oracle prestate passes | accumulated state, side-block or fork resolution, canonical index, restart or unwind residue |
| only at the first checkpoint after a snap start | bodies below the pivot, gap-block candidate snapshot, pivot authentication |
| hash differs, roots equal | header field encoding, `Validators` or `Penalties` bytes, extra data |

## Gates

- 0 mismatches over the whole requested range in sequential mode, or one divergence record for the first mismatch.
- The run log states mode, start point, range, count, commit, pin and host.

## Done

Either "sequential, start S, range X..Y, N blocks, 0 mismatches, commit C, pin P, host H" is posted on the work item, or the divergence record exists and the loop has the next step.
