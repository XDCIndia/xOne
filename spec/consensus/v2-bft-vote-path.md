# V2 vote path: proposals, the voting rule, vote pool and QC formation

## Rule

The V2 protocol is a step machine `Bft::step(chain, durability, event) -> effects` (`xone_consensus::v2::bft`). It reads the chain through the `Chain` trait only (headers a node has executed, the epoch of a block, the candidate pool of a gap, the gap of a timeout), holds no clock, no key and no I/O, and returns typed effects in the order a driver must run them:

`PersistIntent`, `Sign`, `Send`, `SetTimer`, `AdvanceRound`, `Commit`, `Store`, `ScorePeer`, `Forensics`, `Refused`.

**Proposal** (`ProposedBlockHandler`). A block that the node has executed, canonical or on a side branch, is a proposal. In this order: (1) its QC goes through `processQC`; an error ends the handling and no vote is made; (2) the key must be a masternode of the block's epoch (`allowedToSend`); (3) the voting rule; (4) if the rule says vote, `sendVote`. The vote is not tied to the head: a block at the height of the head, executed on a side branch, is handled like any other (xOneGo `ee9b750f`: that driver voted only on the head, so a same-height sibling proposed after a timeout was never voted).

**Voting rule** (`verifyVotingRule`). Vote only if all hold, in this order:

1. `currentRound > highestVotedRound`;
2. `block.round == currentRound`;
3. there is no lock QC, or the round of the QC carried by the block is above the round of the lock QC, or the block extends the lock QC's block (`isExtendingFromAncestor`: walk `block` down by `block.number - lock.number` parents, none for a negative difference, and compare hashes; a header missing in the walk is an error, not a "no").

The rule is a function of explicit inputs (`bft::rule::verify_voting_rule`). It knows nothing about heights: a sibling of a block that timed out is voted exactly when its round is the current round, the node has not voted in that round and the lock allows it.

**A proposal for a later round.** If the proposal's round is above the current round (its TC has not arrived), geth drops the block: rule (1) or (2) says no. Here the block waits in a stash, one block for each of the next 5 rounds, and goes through the handler again when the node enters that round (xOneGo `ee9b750f`: "a stashed sibling is retried once the TC advances the round"). It is the same rule on the same inputs, only later.

**The safety unit.** `Bft::authorize` is the one place that makes `Sign` and `Send`. It calls `safety::decide` on the record (ADR-010) and, on success, emits `PersistIntent` (unless the record already holds the intent: a retry), then `Sign`, then `Send`. The unit repeats the round bound and the lock rule, so a mistake in the rule above is not enough to sign. The driver runs the effects in order and stops the process if the write fails; the machine keeps the new record in memory from the moment it emits the effect, which is safe because no later step runs after a failed write. The record gets `highest_voted_round` from a vote or a timeout; this is stricter than geth (`vote.go:406` does not count timeouts), ADR-010 section 7.

**Own message.** geth runs its own vote through `voteHandler` before it broadcasts (`vote.go:269`). Here the driver feeds each sent vote and timeout back as `Event::Vote` and `Event::Timeout` with no peer; the signature is only known to the driver.

**Vote handling** (`VoteHandler`, `voteHandler`). (1) Round window: `currentRound <= vote.round <= currentRound + 5`, before any signature work (`voteRoundInRange`, #1665). (2) The signer is recovered and must be in the candidate pool of the gap snapshot at `vote.gap_number`; a signature that fails is scored (`ScorePeer: Invalid`) and nothing is pooled. (3) The pool keeps one vote for each signer and round. (4) The QC forms when the distinct signers that voted for this block (same round, gap, number and hash) and are masternodes of its epoch reach `min_signers(len(masternodes), certificateThreshold of the config at the vote's round)`, the block's header is here, and its number and round agree with the vote (`VerifyBlockInfo`). (5) The QC carries one signature for each of those signers and goes through `processQC`. If the header is missing, geth waits for one more vote; here the QC is also tried when the block is executed.

**processQC.** Highest QC: raised if the round is higher, before the header is looked up. Then the header of the certified block must exist (an error otherwise, and the round does not move). Above the V2 switch block: the QC that header carries becomes the lock QC if there is no lock or its round is higher; `commit_blocks` (3-chain, `spec/consensus/v2-commit.md`) may commit and emits `Commit`; `Forensics` sees the commit (`spec/consensus/v2-forensics.md`). Then, if the QC's round is at least the current round, the node enters `round + 1`.

**Pools** (`pool::Pool`). geth keeps one entry for each pool key and recovered signer (`utils/pool.go:44-84`, XDC-07); the key of a vote contains the block, so one signer can still hold one entry for each block of a round, and a flood of different blocks grows the pool. Here a signer holds one entry for each round, whatever it signed. An honest signer votes once in a round (ADR-010), so no honest vote is lost. Memory is at most `rounds * candidates` entries for the votes (the window is 5 rounds ahead and `PoolHygieneRound = 10` behind, pruned at each new round, not by a 60 s job) and `candidates` entries for the timeouts (cleared at each new round). The vote counted for a QC and the QC's signatures are distinct masternodes; signature order is arrival order (geth iterates a Go map, so its order is random).

## Source

- `engine_v2/vote.go:22-85` `VerifyVoteMessage`; `:88-124` `VoteHandler`; `:132-136` the lookahead; `:138-214` `voteHandler`; `:217-299` `sendVote` (`:292-294` the latch, `:297` the broadcast); `:343-401` `onVotePoolThresholdReached` (`:344-363` one signature for each signer, F-2026-19525); `:404-446` `verifyVotingRule`; `:449-462` `isExtendingFromAncestor`; `:465-483` `hygieneVotePool`.
- `engine_v2/utils.go:476-528` `processQC`; `:619-668` `VerifyBlockInfo`; `:758-790` `ProposedBlockHandler`.
- `engine_v2/engine.go:3266-3284` `allowedToSend`.
- `utils/pool.go:35-84` `Pool.Add`; `core/types/consensus_v2.go:75-78` the vote pool key.
- CertiK XDC-07 and F-2026-19525: `pool_signer_dedup_test.go`, `threshold_signers_1552_test.go`.
- xOneGo `ee9b750f` (message and tests only; no Go code ported).

## Networks

All networks with V2 (mainnet 50, Apothem 51, devnet 5551, devnet551 551, net2040).

## Rust

`xone_consensus::v2::bft::{Bft, Event, Effect, Outbound, Chain, HeaderFacts, EpochFacts, Config, rule, vote}`, `v2::pool::Pool`, `v2::cert::{vote_digest, min_signers, unique_signers_with, Scheme}`.

## Vectors

- `conformance/vectors/v2/bft-rule.json` (oracle `crates/xone-consensus/oracle/bft`, command `rule`): 3,027 explicit-input cases of the pinned geth `verifyVotingRule` on 24 main-chain blocks and four forks: lock QC above, extending the lock, no vote (voted, round, lock) and header errors, plus the nine edge combinations of the three round comparisons. `tests/bft_vectors.rs::the_voting_rule_decides_as_geth_on_explicit_inputs`.
- The sibling shape of net2040 619,712 and 619,713 (`sibling` in the same file): parent 619,711, block A at 619,712 in round R, A' at the same height and parent in round R+1, a second sibling A'', a child of A' at 619,713 in round R+2, and a fork block; nine cases with the verdicts of geth. The heights are those of the incident; rounds and hashes are made up, because the archive was not read. `tests/bft_vectors.rs::the_sibling_shape_of_net2040_619712_619713_is_voted_as_geth`; `tests/bft_machine.rs::a_same_height_sibling_after_a_timeout_is_voted_when_its_round_starts` runs it through the machine: A is voted in round 2 and its round times out; A' arrives before the TC and waits; the TC comes and the first effects of round 3 are `PersistIntent`, `Sign`, `Send(Vote)` for A'.
- Pools: `tests/bft_machine.rs::ten_thousand_variants_of_one_signer_keep_one_entry_and_cost_bounded_time` (20,000 valid variants of one signer, votes for 251 blocks and timeouts: one vote and one timeout kept, no effect, 109 microseconds for a message in a debug build, measured; the limit asserted is 1 ms) and `real_signature_variants_that_do_not_verify_cost_a_score_each_and_no_memory` (2,000 bit flips of a real signature).
- Effects: every test calls `assert_persist_before_send` on every effect list: a `Send` is right after its `Sign`, and a `PersistIntent` of the same intent is before that `Sign`.
- Simulation: `sim/` (`sim/README.md`), seeded mutants in `tests/bft_mutants.rs` (lock check, duplicate votes, send before persist).

## Not covered

- Vote equivocation proofs (`ProcessVoteEquivocation`, `DetectEquivocationInVotePool`): the pool drops the second vote of a signer; it does not report it.
- Gap-snapshot lookup, epoch lookup and the header verifier are the `Chain` implementation (XB-11 and the driver).
- The `Initial` / `ReinitBFT` paths for a trusted-checkpoint window (`inCheckpointCatchup`) are a bulk-sync device of the fork and not a rule.
