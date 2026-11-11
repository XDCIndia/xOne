# xone-sim

Test-only simulators of the V2 signer and of the V2 BFT protocol. No other crate may depend on it (`ci/layering.py`).

| module | what | test |
| --- | --- | --- |
| `src/lib.rs` | one validator's vote path through the safety unit: kills at seven points, stale restores, degraded storage (ADR-010, XA-07) | `cargo test -p xone-sim --lib` |
| `src/net.rs` | 4 to 14 validators running the real step machine `xone_consensus::v2::bft::Bft` over a lossy network (issue #35, XB-15) | `cargo test -p xone-sim --release --test bft_seeds -- --nocapture` |

A seed is one run. `SIM_SEEDS=<n>` changes the count (default: 10,000 in a release build, 300 in a debug build). `cargo test -p xone-consensus --test bft_mutants -- --ignored --nocapture` runs the seeded mutants against the vectors and against this simulator.

## What runs

For each seed: `n` in 4 to 14 validators, each with a key, a `Bft` machine, a `MemStore` for its safety record, and the blocks it has executed. Honest validators run the real machine: pools, QC and TC formation, the voting rule, the safety unit (`decide`), the round timer with the per-round exponential config, SyncInfo handling, forensics. The driver in `net.rs` is what XB-23 and the node will be: it runs the effects in order, persists before it signs, loops its own messages back, schedules timers, keeps the highest QC and TC (`Store`) and feeds them back at restart, lets the leader of a round seal one block `mine_period` ticks after the round starts, imports blocks (leader, QC with a quorum, parent, height) and pulls missing blocks from peers.

The signature is a hash stand-in (`Stand`), the one thing that is not the production code: `signer(20) | keccak(secret(signer) | digest)(32) | nonce(8)`, and `recover` ignores the nonce. A validator therefore has any number of distinct valid signatures of one message, as with real ECDSA, and nobody can sign for another. It goes through the `cert::Scheme` seam; real secp256k1 is used in `tests/bft_machine.rs` and `tests/bft_vectors.rs` of `xone-consensus`.

## Fault model (what is permitted)

- **Byzantine validators.** At most `f = floor((n - 1) / 3)` of the `n` validators, chosen by the seed (0 to f; every pair `(n, b)` occurs in the first 3,000 seeds). They sign anything with their own key, in any number of variants; vote for several blocks of one round (equivocation); vote with no regard for the voting rule; time out any round; stay silent; as leader seal two blocks in a round, or a block on an older QC than the highest (the lock attack: an old QC that exists, because they cannot make one); and send SyncInfo with QCs of one signer, with signatures of other validators that do not verify, with 151 signatures, and with a TC of a round far ahead. They know every message at once (they act on the highest QC or TC that exists). They cannot forge the signature of another validator or make a certificate without a quorum of real votes.
- **Crash faults of honest validators.** A kill before the write of the safety record, after the write and before the signature, after the first send, or a store fault (nothing written, written but not synced, sync error, synced but no ack); the machine restarts from the record that the store holds, with the blocks it had, after 1 to 20 ticks. All honest validators are up from 21 ticks before GST.
- **Network before GST.** Each message is lost with a probability of 0 to 44 percent, duplicated with 0 to 19 percent, delayed 1 to `delta_pre` ticks (up to 8), and dropped between the two sides of up to two partitions. Messages to oneself are local and arrive in the same tick. No stale restore of the safety store (XA-07's simulator does that, the fence is outside this item).
- **GST** is a tick between 40 and 179. A validator may be anywhere in its round at GST.

## The quorum

`min_signers(n, 0.667)`: the smallest `k` with `float64(k) >= float64(n) * 0.667` (geth `verifyQC`, `utils.go:373`).

| n | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| f | 1 | 1 | 1 | 2 | 2 | 2 | 3 | 3 | 3 | 4 | 4 |
| quorum q | 3 | 4 | 5 | 5 | 6 | 7 | 7 | 8 | 9 | 9 | 10 |

For every row `2q - n > f`: two quorums meet in more than f validators, so in an honest one that votes once in a round; and `n - f >= q`: the honest validators alone are a quorum. A test checks both (`the_quorum_of_every_validator_count_is_safe_and_reachable_with_f_faulty`). Certificates count distinct recovered signers, never entries or signature bytes.

## Clock and timer model

Time is a count of ticks; one tick is one second of the config. The clocks of all validators are the same: a timer set for `d` seconds fires after `ceil(d)` ticks, with no drift. The delay is the real `Timer` of the machine, `period * base^min(round - highest QC round, max exponent)` of the config row of the round; a seed has one row or two (the second from a round between 20 and 59), with a period of 10, 12, 15 or 20, a base of 1, 1.5 or 2, a max exponent of 0 to 4, a `timeoutSyncThreshold` of 2 to 4 and a `minePeriod` of 1 or 2. The leader of a round (`masternodes[round % epoch % n]`) seals `minePeriod` ticks after it enters the round (at least 1). There is one epoch, so no epoch switch is simulated (`spec/consensus/v2-epoch-switch-and-gap-snapshot.md` and the vectors of XB-11 cover it).

## The post-synchrony delivery bound

From GST, with no exception: no loss, no duplicate, no partition, and every message between two validators arrives after 1 to `delta` ticks, `delta` in {1, 2} (Byzantine validators may send or not, but what they send is delivered within `delta`). A validator that restarts has the blocks it had. Every `SYNC_EVERY = 5` ticks each validator pulls from every honest peer the blocks it lacks (before GST from one random peer, with the loss above), so a block that an honest validator holds reaches every other within 5 ticks plus `delta`. The sealing leader needs that to build on the highest QC; this is the "responsive correct leader" condition.

## What is asserted

On every step:

1. **No conflicting finalized blocks.** Every block committed by any honest validator (the `Commit` effect) is an ancestor or a descendant of every other (`on_commit`), across rounds, restarts and forks.
2. **No two quorums in a round.** No two blocks of one round have a quorum of distinct voters (`World::note_vote`). Votes are counted from every signature that left any validator, honest or Byzantine.
3. **One signature per kind and round.** An honest validator never signs two digests of one kind in one round, across crashes; never votes in a round at or below a round it timed out; never signs a vote that does not extend its lock when the QC of the proposal is not above the lock (checked against the real ancestry, not against the machine's own test).
4. **Persist before sign.** A `Sign` is preceded in the effect list by the `PersistIntent` of the same intent, or the record that the store acknowledged already holds it (a retry).
5. **No round without a certificate.** No honest validator is in a round above the round of the highest QC or TC that exists plus one (a forged or under-threshold SyncInfo, or duplicate votes counted as signers, would break this). Every QC an honest validator keeps has a quorum of distinct masternode signers.
6. **Bounded pools.** The vote pool holds at most `(lookahead 5 + hygiene 10 + 2) * n` messages and the timeout pool at most `n`.
7. **Honest peers are never scored.** Every `ScorePeer` names a Byzantine validator.
8. **No forensics event.** With at most f Byzantine validators two committed blocks do not conflict, so any `Forensics` effect is a failure.

**Liveness is measured only under the declared assumptions**: after GST, with the delivery bound above and every honest validator up. `base` is the highest block number committed by any honest validator at GST. The run passes if every honest validator commits a block above `base` within `bound` ticks after GST. The bound is derived from the actual timeout schedule of the seed, not promised as a fixed number of rounds:

```
T_max       = the largest delay of Timer over exponents 0..max_exponent, over the rows of the seed, rounded up
sync_wait   = 2 * sync * T_max + 4 * delta + 2 * (SYNC_EVERY + delta)
failed_round = 2 * T_max + 3 * delta + mine_period
bound       = sync_wait + (n + 3) * failed_round
```

`sync_wait`: validators that are behind are brought to the round of the others by a SyncInfo, which a validator sends after `timeoutSyncThreshold` timeouts of one round; twice, once for the round and once for the highest QC that a leader needs. `failed_round`: a round with a Byzantine leader, or an honest leader with a stale QC, ends when a quorum of validators has timed out; a validator that entered the round late has missed the timeouts sent before, and gets them again one timer period later, so two periods. A cycle of the round-robin leaders has `n` rounds of which at most `f` have a Byzantine leader, and three consecutive good rounds commit (the 3-chain rule); `(n + 3)` failed rounds is a bound for that. It is loose on purpose: the measured recovery is in the PR, as a share of the bound. This is not "3 rounds".

A run goes on for 100 ticks after the recovery so that the safety checks see the steady state; a run that has not recovered at `GST + bound` fails.

## Mutants

`xone-consensus/tests/bft_mutants.rs` edits the source, runs the tests, restores the files. The simulator catches: the lock check dropped from the voting rule and from the safety unit (the vote oracle of item 3 and a conflicting commit), duplicate votes counted (a Byzantine validator's signature variants make a QC with no quorum: item 5), a signature sent before it is persisted (item 4), a SyncInfo that is accepted when it does not verify (item 5). The vectors and `tests/bft_machine.rs` catch the rule-level mutants (the lock check in the voting rule alone is also caught by the vectors; the simulator does not notice it, because the safety unit repeats the check).

## Limits

- One epoch; no epoch switch, penalties or masternode changes.
- The signature scheme is a stand-in; real ECDSA is in the vectors and the machine tests.
- Block production is simplified (XB-23): the leader seals one block a round on its highest QC; a block is valid if its leader, QC, parent and height are right. The leader's own guard against a second block in a round is a set in the simulator, not the safety record.
- The disk is `MemStore` with fault points, not the file store of ADR-005 and ADR-010 section 4 (Gate B). Stale restores and two hosts on one key are not simulated here.
- The clocks do not drift.
- A message that arrives while the validator is down is lost; the validator then relies on the pulls and on the next messages.

## What the simulator found

- Seed 436 (first 10,000-seed run): a validator restarted on the first V2 block kept the placeholder highest QC (geth raises a QC only by round, and that block carries the round-0 QC), so the SyncInfo it sent named no block and every peer refused it; the others, who had missed a TC, never advanced. Fixed in `Bft::process_qc` (the first QC replaces the placeholder), with a machine test.
- Seeds 806 and 1983: a QC formed from votes, or a TC formed from timeouts, is in no block until a leader includes it. A validator that formed it, moved on, timed out in the next round, crashed and restarted without it was a round behind its own record; ADR-010 section 7 refuses a timeout below the record, and with a quorum of exactly the honest validators the others needed that timeout. The machine now emits `Store` (the highest QC and TC); the driver keeps it and feeds it back at start. This also puts a question to the owner of ADR-010 open question 1: the stricter timeout rule is what turns the loss of a certificate into a stop; geth, with no such bound, does not stop.
