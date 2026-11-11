# ADR-010: Signer safety (anti-equivocation state)

Status: proposed (Gate B and D design review). Nothing here is proven on a real disk or on two hosts. Issue: XDCIndia/xOne#12 (XA-07). Depends on ADR-005 (XA-06).
Code: `crates/xone-consensus/src/v2/safety.rs` (pure rules, record bytes, fence), `crates/xone-db/src/safety.rs` (store trait, in-memory fault store), `sim/` (simulator). Test plan: `tests/signer/PLAN.md`.

## 0. Sources

- **[G file:line]** geth-XDC `XDCIndia/go-ethereum` at the pin named in issue #12 (`6fc8e8c2088b7f18264f5663036669d04fa7becf`). The copy read is a plain source tree without `.git`; its lines agree with the issue's links (`vote.go:292-297`, `timeout.go:225-296`). Paths are under `consensus/XDPoS/engines/engine_v2/` unless they start with `core/` or `params/`.
- **[A file:line]** Aptos `safety-rules`, `aptos-labs/aptos-core` main at `c60a164e56d06d5a3681aa3d58d4c4aaa093946c`, `consensus/safety-rules/src/`. Read through the GitHub API on 2026-11-11.
- CometBFT's signing discipline is named by the issue. I did not read it.
- **[D]** a decision of this ADR. **[U]** not verified.

## 1. What geth does today, and what is missing

| # | Behaviour at the pin | Source |
|---|---|---|
| G1 | The voting rule: no vote when `currentRound <= highestVotedRound`; the block round must equal the current round; vote if there is no lock QC, or the proposal's QC round is above the lock round, or the block extends the lock block | `vote.go:404-446` (`isExtendingFromAncestor` at `vote.go:449-462`) |
| G2 | `highestVotedRound` is latched after the local handler accepted the vote and before broadcast, with the round being voted (not `currentRound`), and written with `WriteBFTHighestVotedRound` | `vote.go:281-298` |
| G3 | The signature is made before the latch and the write (`signSignature` at `vote.go:234`). It stays in the process until broadcast, so it cannot leak first, but the order is sign, then write | `vote.go:233-298` |
| G4 | The write is a plain `Put`; the DB is pebble with `NoSync`. The record lives in chaindata, so restoring chaindata lowers it | `core/rawdb/accessors_xdc.go:125-131`; `ethdb/pebble/pebble.go:234` |
| G5 | A missing or malformed value reads as 0, called a "safe default". 0 is the lowest value, so a wiped record allows any vote | `core/rawdb/accessors_xdc.go:114-122`, `engine.go:581-596` |
| G6 | The lock QC is persisted (`WriteBFTLockQC`) when `processQC` raises it. It only rises: new lock round must be above the old one | `utils.go:499-510`, `core/rawdb/accessors_xdc.go:168-181` |
| G7 | `highestQuorumCert` rises by round and is not persisted. `highestTimeoutCert` rises by round and is not persisted | `utils.go:479-486`; `timeout.go:474-478` |
| G8 | `sendTimeout` signs for `currentRound` and writes no record. `OnCountdownTimeout` calls it again at every timeout period, so the same timeout is sent several times per round | `timeout.go:225-297`, `timeout.go:299-329` |
| G9 | The timeout payload is `(round, gapNumber)`, not `round` alone. `gapNumber` comes from the epoch of the current head, by one of two branches (epoch switch at the round, or the head's epoch info). If the head changes inside one round across an epoch boundary the payload may change **[U, not tested]**. The issue asks to confirm that the payload is deterministic per round "as the reason" no record is written: it is deterministic only for a fixed head | `timeout.go:230-264, 267-270` |
| G10 | The leader's guard is `highestSelfMinedRound`: no proposal for a round `<=` it. It is persisted with a plain write | `mining.go:24-31`; `engine.go:134-143` |
| G11 | A timeout does not change `highestVotedRound`. After a timeout in round r a proposal for r can still be voted until a TC moves the round | `timeout.go:225-297` (no write), `vote.go:406` |
| G12 | Timeout length is `Base^min(exp, MaxExponent) * TimeoutPeriod`. Current mainnet row: 60 s, Base 1.0, MaxExponent 0 | `params/config.go:2491-2496`, `params/config.go:2551-2558` |

Reference pattern, [A]: a timeout for round r is refused if r is below `last_voted_round`, raises `last_voted_round` to r, and the record is written before the signature is made (`safety_rules_2chain.rs:37-49`; write at 47, sign at 49). A vote needs a round above `last_voted_round` (`safety_rules.rs:213-231`). A second vote request for the same round returns the stored vote (`safety_rules_2chain.rs:68-73`).

## 2. Signer identity

The signer is the address of the validator key (`x.signer`, set with the sign function). The record stores the 20-byte address. At start the unit compares it with the configured address and refuses on a mismatch (a record of another key is never adopted). One store directory holds one identity. A new key means a new directory and a new record; there is no migration of rounds between keys.

## 3. The safety record

All integers little endian. `SafetyRecord::encode`, 339 bytes (format version 2, XB-23; version 1 had three slots and 299 bytes):

```
off  len  field
0    4    magic "XSR1"
4    2    format version (1)
6    20   identity (address)
26   8    seq: count of writes of this record. Not a clock
34   8    highest_voted_round: highest round of a vote or a timeout
42   160  signed[vote], signed[timeout], signed[propose], signed[seal_v1]: round u64 | digest 32 (round 0 = none)
202  1    lock flag (0 or 1)
203  48   lock QC: hash 32 | round u64 | number u64 (all zero if the flag is 0)
251  48   highest QC, same layout
299  8    highest_tc: round of the highest TC
307  32   keccak256 of bytes 0..307
```

- The checksum is keccak256 (same choice as ADR-005 section 6.2). Decode checks length, magic, version, checksum, and that an unset lock is all zero. Any failure is a refusal to sign, never a reset to zero (this removes G5).
- Only rounds, hashes and numbers of QCs are kept. The certificates stay in chaindata. `BlockRef` has the fields of `BlockInfo` (XB-04, branch `feat/xb-04-header-vectors-v2-types`); it is replaced by it when that merges.
- **Monotonic invariant:** every field of a new record is `>=` the same field of the old one (`SafetyRecord::dominates`). `decide`, `observe_qc`, `observe_tc` and `sight` can only raise. A test and the simulator check it.
- `per-type highest signed round`: the three `signed` slots. `highest_voted_round` is separate because a timeout raises it and a vote needs a round above it (section 7).

## 4. The store

Decision [D]: its own directory `datadir/safety/`, not inside `chain.mdbx`, and not a table in an MDBX environment of its own.

- Separate from chaindata: restore, snapshot import, `SetHead` and unwind work on `chain.mdbx`, `static/` and `journal/` (ADR-005 section 1). A tool that replaces those must not be able to replace the record. The safety directory is outside every backup set of those tools; a full-disk image or VM snapshot is the case the fence (section 8) covers.
- A file, not MDBX: the record is one 339-byte value with one writer. MDBX would add an mmap environment, its sync-mode choice (ADR-005 section 4) and a second copy of its failure modes for one key. A file needs one fsync per write.
- Layout: `safety.rec` of two slots of 1024 bytes, created at `init`, fsynced with its directory once. A write goes to the slot that does not hold the newest valid frame, then `fsync`. A frame is `store_seq u64 | intent 41 | record_len u16 | record | keccak256 of the preceding bytes`. A torn write damages only the slot being written; the other slot is the last acknowledged state. Start-up takes the valid frame with the highest `store_seq`. This needs only write ordering around `fsync`, not atomic `rename` (ADR-005 N7 is `[U]`).
- No valid slot in an existing file: `StoreError::Corrupt`, refuse. Missing file: refuse until the operator runs `xone signer init`, which writes a zero record for the identity. A zero record is treated as unproven (section 8).
- `load()` syncs the file before it returns a frame, so a frame that was only in the page cache after a crash is durable before anything relies on it.
- Failure policy: any error of write or sync is fail-stop for signing, as ADR-005 section 9: the unit stops signing, the process exits, `FSYNC_FAILED` with the boot id is written (best effort) and a restart in the same boot refuses. No retry of `fsync`.
- An exclusive `flock` on `datadir/safety/LOCK` keeps a second process on the same host out.

`SafetyStore` has two methods: `load()` and `persist_intent(intent, record) -> Ack`. `Ack` is made only after the sync returned. The in-memory store (`fault-injection` feature, never in a release artifact) can fail before the write, after the write, at the sync, or after the sync without an ack.

## 5. Never lowered

The record changes only through `decide`, `observe_*` and `sight`, which only raise it. `xone-db` exposes no function that writes, truncates or replaces it from the chain side. In particular:

- `BlockWriter::unwind_to`, operator `SetHead`, datadir restore, and snapshot import do not read or write `datadir/safety/`.
- Any tool that copies `datadir/safety/` back from a backup is an operator action outside the node. The node cannot tell, so it treats every start as unproven (section 8).
- After a chain rollback the record stays high: the signer may refuse to vote in rounds it re-visits. That is the intended cost.

## 6. Write-ahead protocol

```
event -> consensus step -> Intent(kind, round, digest) + chain facts
      -> fence open?            no: refuse
      -> decide(record, intent, durability)
           refuse: nothing written, nothing signed
           same record back (a retry): skip the write
      -> persist_intent(intent, new record)   write + fsync
      -> Ack
      -> sign
      -> send
```

Rules: no signature exists before the `Ack`. The record in memory is replaced only after the `Ack`. Any store error stops signing until restart. The driver (`xone-node`, not in this item) owns this order; the simulator has its own driver with the same order.

| Crash point | State after restart | Result |
|---|---|---|
| before the write | old record | nothing signed; the consensus step re-delivers |
| written, not synced | old or new record (the load syncs it) | nothing signed |
| synced, no ack | new record | nothing signed; a retry of the same intent gives the signature; a different digest for that round is refused |
| ack, before sign | new record | same: the retry signs |
| signed, before send | new record | the signature is dropped; the retry signs the same bytes (deterministic signer) |
| after send | new record | a retry is a replay |

**Idempotent retry.** `decide` returns the input record unchanged when the intent equals the stored slot of its kind (same round and digest). The unit then signs again without a write. The signer must be deterministic (RFC 6979 ECDSA, as geth's `crypto.Sign`) so that the retry gives the same signature. A different digest for the same round is `Refusal::Conflict`. A retry of a vote after a timeout for the same or a later round is refused (section 7), so "the same signature or none" holds. Degraded storage refuses a retry too.

## 7. Rules and the timeout rule

`decide` (the voting rule inputs are from G1):

- Round 0 is never signed (it is the initial value of every guard, `engine.go:235`, `vote.go:406`).
- Vote: refuse if the round is `<=` `highest_voted_round` (G1, `vote.go:406`); refuse if the proposal does not extend the lock and its QC round is not above the lock round (`vote.go:422-443`; no lock means vote, `vote.go:422-424`). `extends_lock` and `qc_round` are computed by the consensus step from the chain (`isExtendingFromAncestor` reads headers and can fail; a failure there is the step's refusal, not a vote). The block round equal to the current round (`vote.go:414`) is a consensus-step check; the record holds no current round.
- One signature per kind and round: a second digest for the same kind and round is `Conflict`.
- Propose: refuse a round `<=` the highest proposed round (G10, `mining.go:26`).
- QC and TC observations: highest QC and highest TC rise by round (G7); the lock rises to the candidate QC if its round is higher (G6). They are written with the next intent; a crash loses only observations that no signature relied on, and they are re-derived from the chain.

**Timeout rule (decision [D]).** Signing a timeout for round r:

1. is refused if r is below `highest_voted_round` (no timeout under a round already voted or timed out: [A] `safety_rules_2chain.rs:37-42`);
2. records `signed[timeout] = (r, digest)` and raises `highest_voted_round` to r, in the same write that precedes the signature ([A] `safety_rules_2chain.rs:43-49`);
3. therefore blocks every later vote for a round `<= r` (`TimedOut`);
4. is allowed after a vote for the same round r (a node may vote, then time out), and a repeat of the same timeout is a replay (G8: geth sends it every period);
5. a repeat with another digest for round r (another `gapNumber`, G9) is `Conflict`. The cost is liveness of that node in that round only;
6. does not block a proposal (`Propose` has its own record).

A vote that was authorised and written but never sent is not released after a timeout for the same or a later round was signed. The rule is stricter than geth (G11). It changes only what this node signs, not what other nodes accept. Open question 1.

## 8. Cross-host fencing and stale restore

Decision [D]: a **conservative quiet window with observation**, not a lease or epoch token. Every start (cold start, crash restart, failover, restore, `init`) is unproven. The rule, as implemented by `Fence` and `SafetyRecord::sight`:

1. At start the fence is closed: no signature of any kind.
2. The window is `W = 2 * TimeoutPeriod * Base^MaxExponent` of the V2 config for the highest known round (G12). Mainnet now: `2 * 60 * 1.0 = 120 s`. It is counted in ticks of 1 s of a monotonic clock. A tick counts only if a valid gossip message (vote, timeout, QC or TC verified against the masternode set) from another peer arrived in that second. A partitioned node does not open.
3. During the window every message verified as signed by our key that this process did not emit (vote, timeout, a QC or TC signature, the validator of a sealed header) is checked with `SafetyRecord::sight(kind, round, digest)`. If it is above the record (round above the slot of that kind, or the same round with another digest) the record is raised to it and persisted, and the window restarts from zero (`Fence::sighted`). A sighting that the record already covers changes nothing.
4. When `W` ticks pass with no unexplained sighting, the fence opens. A sighting after that closes it again by the same rule.

Why this works for the two cases:

- **Second host with the same key.** A live signer emits a vote or a timeout at least once per timeout period (G8, `timeout.go:299-329`; votes per round), so within `W` the new host sees it, raises its record, restarts the window, and never opens while the other lives. A host that starts beside a live one yields. After the first host is stopped, the second opens `W` after its last sighting, with a record raised to what the network saw.
- **Stale-backup restore.** The restored record is below what the old instance signed. Every signature it sent is on the network and is seen in the window (QCs, votes, timeouts, sealed headers carry our signature). The record is raised above all of them before the fence opens. This is the proof of "a newer record": the restored record is not used until the network has shown no signature of ours above it for `W` connected seconds. A wiped record (zero) is the same case.

Why not a lease or an epoch token in the record: a token in a local record cannot stop another host unless a shared service checks it at every signature. That service is a new dependency and a new way to stop all validators; a lease still needs a wait of one lease period after a holder dies, which is this window. A token also does not help a restore, which brings back an old token. The record carries `seq` for ordering only. If the owner wants a lease later, it is another gate in front of `Fence::open`, no change to `decide`.

Residual risks (not claimed to be covered; the simulator covers none of them):

- R1. A signature that reached nobody (sent into a partition and never seen) leaves no trace. A restore from before it can sign another digest for that round. `sim::tests::unseen_signature_is_the_residual_risk` shows it. Mitigation is operational: do not restore the safety directory; prefer a new key.
- R2. Two hosts that open at the same moment, or a host that starts while the other is partitioned away from it, can both sign until a sighting arrives. A random extra delay in `[0, W)` on start lowers the chance of the first case and is not specified here. Open question 2.
- R3. After a restore the first `W` seconds are a blackout of this validator (120 s on mainnet today). The same blackout applies to every ordinary restart, because a restore cannot be told from a restart. Open question 2.

## 9. Interface to XA-06

`BlockWriter::durability()` returns `Durability`. The driver passes it to `decide` for every intent. `Durability::Durable` is the only value that allows a signature; `Degraded(Reconciling | NoSpace | Io | FsyncFailed)` is `Refusal::Degraded(cause)` for every kind, a retry included. A signature may refer only to a head of a published generation (ADR-005 sections 2 and 11): the consensus step builds the intent only from published data. The safety store has its own failure state (section 4); it does not use `Durability`.

## 10. Layering: only the safety unit reaches the signer

- The key and the sign function live in a crate `xone-keys` (later item). Its `Signer::sign` takes an `Authorized` value that only `xone-consensus::v2::safety` can build, and that is built only after an `Ack` (or a replay). The type makes "sign before persist" a compile error outside the unit.
- Only the crate that holds the safety unit may depend on `xone-keys`. The workspace check (`tests/signer/PLAN.md`) reads `cargo metadata` and fails for any other dependent, direct or through a re-export.
- The simulator uses a stand-in signer inside `sim/`. `xone-sim` is not a dependency of any shipped crate; the release-tree check from `xone-rs-release` must show no `fault-injection` feature and no `xone-sim` in `bin/xone`.

This item does not add `xone-keys` or `Authorized`. The simulator's own driver follows the order of section 6, and its mutants show what breaks without it.

## 11. What the simulator proves

`sim/` runs one validator from a seed (SplitMix64): proposals with random chain facts, timeouts with an occasional different gap, leader proposals, QCs, TCs, ticks, kills at seven points (before the write, written not synced, fsync error, synced no ack, after the ack, after sign, after send), spontaneous restarts, stale restores (an older durable record, or a wiped one), partitions (inbound messages dropped), degraded storage, and re-delivery of earlier intents. Oracle: the set of emitted signatures. Failures: two digests for one kind and round; a new vote for a round at or below an earlier emitted timeout; a record that went down across a non-stale restart.

Seeds and runtime are in the PR. Seeded mutants: no write before sign (caught), no fence after a stale restore (caught). Two mutations of `decide` were also checked by hand and caught (no timeout bump; `<` instead of `<=` in the vote bound). The model assumes that a signature that left the node is seen during the quiet window (R1 is outside it).

## 12. Open questions for the owner

1. **Timeout rule.** Approve the stricter local rule of section 7 (a timeout raises the vote bound; an unsent authorised vote is not released after a timeout)? geth does not do this (G11). It cannot make this node's signatures conflict with any other rule, but it can cost one vote in a round.
2. **Quiet window.** Approve `W = 2 * TimeoutPeriod * Base^MaxExponent` (120 s now) on every start, or a shorter window for a start that follows a clean stop? A clean-stop marker is itself part of what a restore can bring back. Add a random start delay (R2)?
3. **Store form.** Two-slot file in `datadir/safety/` (section 4) instead of an MDBX environment: approve?
4. **Lease.** Is a shared lease service wanted for validators with failover? It adds a dependency and a way to stop signing.
5. **Round numbering.** Is round 0 never signed correct at the V2 switch? I did not check which round the first V2 block carries (`GetRoundNumber` returns 0 at or below the switch block, `utils.go:801-810`) **[U]**.
6. **Timeout payload.** G9 shows the payload depends on the head. Confirm with the geth owners whether two different gaps in one round can happen on mainnet; the rule refuses the second one.
7. **Chain facts.** `extends_lock` and `qc_round` come from the consensus step. Is it accepted that the safety unit trusts them (a bug there is a consensus-step bug), or should the unit re-check ancestry from headers it reads itself?
8. **Key custody.** Who owns `xone signer init` and the rule "new key, new directory"?

## 13. Gate B and Gate D

See `tests/signer/PLAN.md`. Gate B: the real file store under the ADR-005 fault matrix and the same kill points; layering fixture. Gate D: two hosts with throwaway keys on a local devnet, restore and failover cases, producer qualification. Any conflicting signature or lowered record blocks Gate D.

## 14. Addendum (XB-23): the unit, the V1 seal slot, the join

- `xone_consensus::unit::SafetyUnit` is the section 10 unit: it owns the key (`Backend`: the local key, or a remote signer with its fencing token), decides, writes, then signs. A signature needs an `Authorized` value that only that module can build (`compile_fail` doctest); `xone-builder` signs only through the `Sealer` trait, and `ci/layering.py` still allows only `xone-consensus` and `xone-node` to depend on `xone-signer`. A sign transaction goes through the unit and does not change the record.
- A fourth slot, `Kind::SealV1`, holds V1 seals and M2 co-signatures by block number. A V1 height and a V2 round are different counters; in one slot a V1 height (millions on Apothem) would refuse every V2 round after the switch. The slot does not raise `highest_voted_round`. Format version 2.
- `SafetyRecord::join` (fieldwise maximum): the BFT machine keeps its own copy of the record for its observations (QCs, TC, lock), the unit holds every signature including proposals. `drive` hands the machine's record to `Sealer::sign_joined`, which joins it before the write, so the machine's later writes cannot lower a proposal round.
- `FileSafety` (`xone-node`) is the two-slot file of section 4, with an exclusive lock. Still not run under the ADR-005 fault matrix (Gate B).
- The quiet window of section 8 is a parameter of `SafetyUnit::open`; a private devnet passes 0 (no second signer by construction). Public networks are refused by the producer in this slice.
