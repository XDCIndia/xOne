---
name: xone-rs-mint-gate
description: The mint protocol for the xOne Rust client (Gate E, public rollout), on a borrowed seat or with its own validator, on net2040 and then Apothem. One signer at a time, fast abort, seals verified by timestamp, child link and the client's own log, and a soak counted by eligible turns, elapsed time, restarts, epoch changes and fault cases. Use only when the owner has approved a mint test in the current conversation, and the task says mint test, borrowed seat, seat rotation, cutover, enable mining, start the producer, own validator, or verify seals.
---

# xone-rs-mint-gate

A mint test changes a live network. Nothing in this skill starts without an owner approval that names the network, the validator address and the window.

## Operating rules

1. **Owner approval first**, for each test. An agent message is not an approval.
2. **One signer at a time.** A validator key runs in exactly one process on the whole fleet. Stop the current signer, confirm 0 processes, then start the next. Two signers on one key is a safety fault.
3. **One cutover at a time** across all clients and hosts.
4. **Peer in before taking the seat.** The candidate is at tip, with 8 or more peers, before the switch.
5. **Fast abort.** On the first missed leader turn, stop within 45-60 s and return the seat. Never wait out a 300 s dead slot.
6. **net2040 before Apothem.** Mainnet minting is out of scope.
7. **Keys.** Keystore signing only; no plaintext key in a unit, a flag or a log. Remove staged key copies after the test.
8. Never stake, propose, resign or vote for a validator from this skill. Those are owner actions.
9. Do not stop or restart another producing validator for this test.

## Inputs

- Owner approval: network, validator address, window.
- The candidate build with its stamp (`xone-rs-release`).
- The rollback signer (geth, or xOneGo v1.4.4 for `0x68a2`), installed and at tip.
- RPC of an independent node on the same network for verification.

## Pre-flight (all must be true)

- [ ] Gates B, C and D are recorded as passed for this exact build (gate DAG: no public producer test before them). Gate D includes `xone-rs-durability-signer-gate` green.
- [ ] Follower parity: `xone-rs-diff-exec` in sequential mode shows 0 mismatches over at least 5 epoch switches at tip with this exact build.
- [ ] The consensus checklist rows B1-B4 and V1-V9 are closed (`xone-rs-consensus-parity`).
- [ ] The safety record exists, is separate from chain state, and survives a restart, an unwind, `SetHead` and a stale-backup restore (highest voted round does not go back).
- [ ] Exactly one signer boundary for this key, with a fence that works across hosts (a local process check is not enough).
- [ ] The validator is in the masternode set and not in the penalty set.
- [ ] Candidate at tip, 8+ peers, lag ≤ 1 block.
- [ ] Host disk below 85%; logs rate-limited.
- [ ] The rollback signer is at tip and its start command is ready.

## Steps

1. **Record the flip block.** Note the head number and time before the switch.
2. **Stop the current signer.** Confirm 0 processes with the key. Wait for one block.
3. **Start the candidate as producer.**
4. **Watch the leader turns.** Compute the turns from `round % 900 % N` on the masternode list in force.
5. **Verify each seal with all three checks:**
   - *Timestamp:* the block is stamped after the flip. A block sealed before the flip does not count.
   - *Child link:* the next block's parent hash is this block, and the block is canonical on the independent node.
   - *Own log:* the candidate's log has the seal line with the same number and hash.
6. **Abort rule.** A missed turn, a wrong-root error, a rejected own block, or a vote on two blocks of one round: stop the candidate at once, start the rollback signer, record what happened.
7. **Borrowed seat:** 3 verified seals is a smoke test, not reliability evidence. Roll back to the original signer after it. **Own validator:** continue to the soak.
8. **Soak (own validator).** Count, do not sample a window:
   - **Eligible turns:** every round where the candidate was the leader, from the formula in step 4. Report the number.
   - **Minimum soak:** elapsed time, eligible turns, epoch changes crossed, restarts, and fault cases (below) must each reach the minimum set in the Gate-E issue. Proposed starting values for the owner to confirm: at least 200 eligible turns, at least 72 h, at least 3 epoch changes, at least 5 restarts.
   - **Restarts:** planned restarts, including one inside an epoch and one near an epoch switch, with the safety record checked after each.
   - **Fault cases** that are safe on a public net: a restart during a leader turn, a brief peer loss, a timeout round caused by another validator, a catch-up after a pause. Destructive faults (kill at each safety boundary, stale restore, handoff) run on the local devnet in Gate D, not here.
   - **Every miss is recorded with a cause:** candidate fault, network or peer, other validator, host or environment, unknown. A miss with cause `unknown` blocks the gate.
   - **Timeouts:** at least one timeout vote sent and seen.
9. **After.** Confirm the validator is in the set, not penalized; the other signer is back as a follower with no key; staged key copies are removed.

## Reading the result

Zero misses is an observation, not a guarantee. With `n` eligible turns and 0 misses, the 95% upper bound on the true miss rate is about `3/n` (rule of three): 200 turns bound it near 1.5%. A report states the bound next to the count. The old 1,800-block window gave only 17-20 turns per client; it is not enough.

## Gates

- Borrowed seat: 3 of 3 seals pass all three checks; 0 missed turns; rollback clean. Smoke only.
- Own validator: the soak minimums are met; all misses have a cause and none is a candidate fault or `unknown`; memory within the target (a workload-specific figure, process, page cache and cgroup reported separately); 0 wrong roots; 0 double signs.

## Done

A report on the Gate-E issue: network, validator, build stamp, flip block, the sealed block numbers with hashes, eligible turns versus seals with the `3/n` bound, elapsed time, restarts, epoch changes, fault cases, every miss with its cause, timeouts seen, and the state after rollback. A failed test has the same report plus the abort reason.
