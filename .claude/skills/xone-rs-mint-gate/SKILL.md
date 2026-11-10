---
name: xone-rs-mint-gate
description: The mint protocol for the xOne Rust client, on a borrowed seat or with its own validator, on net2040 and then Apothem. One signer at a time, fast abort, and seals verified by timestamp, child link and the client's own log. Use only when the owner has approved a mint test in the current conversation, and the task says mint test, borrowed seat, seat rotation, cutover, enable mining, start the producer, own validator, or verify seals.
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

- [ ] Follower parity: `xone-rs-diff-exec` shows 0 mismatches over at least 5 epoch switches at tip with this exact build.
- [ ] The consensus checklist rows B1-B4 and V1-V9 are closed (`xone-rs-consensus-parity`).
- [ ] The safety record file exists and survives a restart (highest voted round does not go back).
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
7. **Borrowed seat:** after 3 verified seals, roll back to the original signer. **Own validator:** continue to the soak.
8. **Soak (own validator).** At least 1,800 consecutive blocks with 100% of leader turns sealed, at least one timeout vote sent and seen, and one epoch switch crossed.
9. **After.** Confirm the validator is in the set, not penalized; the other signer is back as a follower with no key; staged key copies are removed.

## Gates

- Borrowed seat: 3 of 3 seals pass all three checks; 0 missed turns; rollback clean.
- Own validator: the soak passes; memory within the target; 0 wrong roots.

## Done

A report on the milestone issue: network, validator, build stamp, flip block, the sealed block numbers with hashes, turns versus seals, timeouts seen, and the state after rollback. A failed test has the same report plus the abort reason.
