# xOne execution plan: goal, workflow, loop (2026-11-11)

Status of the backlog on this date: audit issues XA-00..XA-15 (#5 tracker, #6-#20) are filed. The build
backlog XB-02..XB-29 (markers `<!-- xone-build:XB-NN -->`) is being filed from the plan in #1. The xOneGo
tree and its open defects were compared against both sets; no top-level outcome is missing. The remaining
gaps are acceptance rows inside existing issues (regression vectors for xOneGo #669-#733, the exact
`XDPoS_*` RPC method set, the `run-xone.sh` and `/etc/xone/<net>.env` operator contract).

## Goal

One binary, `xone`, that follows Apothem and mainnet at 0 mismatches against geth-XDC, survives any stop,
never double-signs, and can be deployed the way xOneGo is deployed today (systemd unit, env file, ethstats,
`XDPoS_*` RPC). Rust, revm, alloy and MDBX; geth-XDC at an exact pin is the oracle.

## Workflow (one work item, one branch, one PR)

1. Pick the next ready issue from #5 (all `Blocked by` closed). Re-pin geth-XDC; record the SHA in the PR.
2. Establish failing evidence: a vector from the archive or a property that fails on the current tree.
3. Implement one bounded change in the owning crate. Dependencies point upward only
   (`xone-primitives` <- `xone-chainspec` <- `xone-state`/`xone-evm` <- `xone-consensus` <- `xone-db`
   <- `xone-sync`/`xone-net` <- `xone-rpc` <- `xone-node` <- `bin/xone`).
4. Targeted checks: `cargo fmt`, `cargo clippy -D warnings`, `cargo test` for the crate.
5. Gate for the item: differential (xone-rs-diff-exec), fork boundary (xone-rs-fork-boundary-gate),
   restart/unwind (xone-rs-restart-unwind-gate) or durability/signer, as the issue names it.
6. Independent review by a second agent that did not write the code; it re-runs the gate.
7. PR with the three repository trailers, the spec page link and the differential result. Owner merges.
8. Tick the item in #5 and record the evidence in the issue.

## Loop

- Restart the loop on any code, vector, spec or dependency change that touches the item.
- Three unchanged failures on one approach: stop, write the escalation in the issue with evidence and
  alternatives. Do not weaken a gate or edit an expected value to pass.
- Full-range replay only at integration boundaries (milestone close, geth re-pin, revm bump).
- A safety fault (wrong root, head ahead of data, lowered safety record, double sign) stops the affected
  rollout; unrelated research continues.

## Order of the first batch

| # | Item | Gate |
|---|---|---|
| 1 | XB-02 workspace + XB-04 header RLP + XB-05 genesis/chainspec (this PR) | 4 genesis hashes equal geth-XDC; fork table has no `Missing` |
| 2 | XA-01 rule register and generated fork table | CI rejects drift from the geth pin |
| 3 | XA-02 wide-balance and revm matrix | every row matches geth without a revm fork |
| 4 | XB-06/XB-08 revm hooks and special transactions | Apothem blocks 0..1800 at 0 mismatches |
| 5 | XA-06 storage publication design, XB-07 state | recovery after every injected fault |

Producer work (XB-23, XB-27) does not start before XA-06, XA-07 and XB-13 pass.
