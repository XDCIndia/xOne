# Gate D rehearsal: the producer among geth-XDC validators (XB-47, issue #47)

What this is: a rehearsal of Gate D (an isolated producer, soaked, with fault cases) on a **private loopback
devnet**: three geth-XDC validators, one xone validator and one xone follower, each validator with its own throwaway
key, all four in the genesis masternode set. No public network, no real key: the keys are made by
`ops/devnet/gen-gate-d.py` (`cast wallet new`) under the work directory and are deleted with it; nothing of them is
in the repository. Gate D is not closed by this document; it records what was run, what broke, what was fixed and
what is still open.

## Setup

- xone: this branch (`feat/xb-47-gate-d-rehearsal`, on `origin/main` 46e8769), release build. geth-XDC
  1.17.10-xdc.21 built from the pin (`conformance/PIN`, `6fc8e8c2`).
- Chain (`ops/devnet/gen-gate-d.py`): chain id 4747, `epoch 30, gap 10`, V2 switch at block 30 (`switchEpoch 1`),
  2 s blocks, V2 timeout period 10 s, every fork in net2040's order compressed below block 160 (homestead and
  TIP2019 at 1 ... Cancun 16, dynamic gas limit 40, Prague 45, Osaka 50, Amsterdam 55, tiered rewards and the
  upgraded penalty at 150). The four keys are the V1 masternodes of the genesis and the only candidates of the
  0x88 contract (owner and voter of themselves, 10M XDC each). The genesis is stamped 10 s ahead of the start:
  geth's validator does not seal while its head is older than its at-tip window (`eth/sync_xdc.go:740-763`,
  `:2955-2975`). Both clients read the forks from the genesis `config` block (xone without `--chainspec`).
- xone is masternode 0: the genesis parent has only an in-turn sealer, and geth seals only after it imported a
  fresh block, so xone seals block 1.
- `ops/devnet/gate-d.sh up WORK XONE GETH` makes the chain, starts the five nodes and meshes them (geth
  `admin.addPeer` from each geth to every other node). Ports below the host's ephemeral range (p2p 12561-12572,
  HTTP 12461-12472), geth on loopback HTTP (its IPC path would pass the 107-byte socket limit under the work
  directory), `--nodiscover`, `--netrestrict 127.0.0.0/8`. xone listens on 127.0.0.1 only (it gets a static peer
  entry that nothing answers, so it binds loopback and dials no real node).
- Turn accounting from chain data only (`ops/devnet/turns.py`, read from a geth node): for every V2 round the
  leader is `masternodes[round % 30 % len]` in the list of the round's epoch (`XDPoS_getMasternodesByNumber`); a
  round with no canonical block is a timeout of its leader. Sign transactions: `ops/devnet/signs.py`. Safety
  record: `ops/devnet/record.py` (read-only decode of `safety.rec`).

## Defects found and fixed (commits on this branch)

Each was found on this devnet, fixed at the cause with the geth-XDC place it follows, and re-run.

| # | Defect | Evidence | Fix (geth-XDC) | Test |
|---|---|---|---|---|
| 1 | **Peer BFT messages were dropped.** The chain service matched `Message::Bft` into `_ => {}`; the producer's machine never saw a peer vote, timeout or SyncInfo, so its highest QC was the one in the tip header (the tip's parent) and `v2_tick` returned silently on "QC block is not the head" in every leader round. This is the net2040 miss (rounds 760369, 760382, build 0771fe10). | Same build with only the routing off: 0 of 7 eligible turns, every round logged `the highest QC is for block N-1 ... not the head N; 0 peer votes received`; the validator was then penalised out of the next epoch. | The follower hands them to the producer, which verifies and handles them in the machine and relays a clean one (`Bfter.Vote/Timeout/SyncInfo`, `eth/bft/handler.go:187-229`). `b4e2ffe` | devnet A/B (below) |
| 2 | **Amsterdam header fields not built.** From Amsterdam every produced block lacked the access-list hash and the empty withdrawals/blob/beacon/requests values; every node, its own import included, refused it (`header sets a post-London field`). net2040 is past Amsterdam (21600). | blocks 58 to 80 of run 2: 8 of 8 turns `not produced`. | `core.FillAmsterdamHeader` (`core/state_processor.go:789-809`, worker `miner/xdpos_worker.go:693-702`), empty withdrawals list in the body; checked by `verifyNoUnsealedFields` (`consensus/XDPoS/xdpos.go:770-797`). `b4e2ffe` | `build::tests::amsterdam_fields_follow_fill_amsterdam_header` |
| 3 | **Gas ceiling 420M** on a chain with a `DynamicGasLimitBlock` row: below the row a V2 block on a 60M geth parent broke the 1/1024 bound. | block 34, run 1: `invalid gas limit: have 420000000, parent 60000000`. | geth's default `GasCeil` 60M (`miner/miner.go:70`); bound `engine_v2/verifyHeader.go:117-123`. `b4e2ffe` | `gaslimit::tests::below_the_row_a_default_ceiling_stays_in_bound` |
| 4 | **Genesis not a V1 epoch switch** in the node history: the first V2 reward of a chain that switches at its first checkpoint had no window. | `block 60 (round 30) not produced: reward: no second epoch switch below block 60` (also in `first-mint-local.md`). | `XDPoS.IsEpochSwitch` (`xdpos_api.go:228-235`) and the reward walk (`eth/hooks/engine_v2_hooks.go:688-718`) count block 0. `b4e2ffe` | `hist::tests::genesis_is_a_v1_epoch_switch` |
| 5 | **Producer sealed with no peers.** Restarted while the leader of the next round, it sealed within its first second with 0 peers; the block reached no one. | run 4, round 311; run 5 R1 with the guard off: round 217. | No proposal or seal until synced: a peer, the chain service's caught-up proof once since the start, no peer announcing a block above our head (`miner/xdpos_worker.go:383-387`). `6ea75ea` | devnet R1/R2 (below) |
| 6 | **No way off our own uncertified head.** When the network left a block of ours (timed out, the next leader built a sibling on the QC block), xone refused every peer for good ("does not know our head"). Same for a V1 same-height fork (run 6, block 17). | run 4: stuck at 340 while the network went on; run 6: stuck at 17. | A peer that does not know our head, or holds it as a side block, is asked for its chain from the committed block (V1: 128 below); a branch that ends above our head is adopted: unwind through `Chain::set_head` (never below the committed block), `HistInner::pop_to`, import, our blocks back if it fails. geth: `keepsV2Head`/`writeBlockAndSetHead` (`core/blockchain.go:2419-2442`), a leader proposes on the highest-QC block (`engine_v2/engine.go:3294-3319`). The safety record is not touched. `6ea75ea`, `3d385ef` | `chain::tests::a_longer_branch_is_found_above_our_committed_block`; devnet R1 |
| 7 | **Sign transactions: pool authority and announcement.** The pool admitted special transactions from this node only (set once), so no geth validator's sign transaction entered an xone block; xone never announced its own, so it reached a geth block once in 42. | blocks 1-640 on `3d385ef`: geth signs in xone blocks 0 of 126, xone signs in geth blocks 1 of 42. | Authority follows the head's candidates (`legacypool.go:924-939`, `eth/backend.go:474-485`); own sign tx sent to the peers (`eth/handler.go` `txBroadcastLoop`). `956767d` | devnet (requirement 3) |
| 8 | **Pending block re-sent before sync.** After a kill -9 in the seal window and an 80 s start (host fsync stall), the restart re-sent the stale pending block at once. | run 7, block 653. | `pending.rlp` is finished only once synced, dropped if the chain moved on. `956767d` | devnet A1 |
| 9 | Follower: a peer whose last announcement is a block we already hold, or that only lags our newest block, was set aside for 60 s; an answer that starts on a block we hold was a ban. | follower `f` 10 blocks behind in run 3. | not set aside. `b4e2ffe`, `3d385ef` | |
| 11 | **Leader did not build the sibling.** When the head had no QC and the highest QC was for its parent, xone (the leader) logged `the highest QC is for block 809 ... not the head 810` and let its round time out; the next geth leader built the sibling and xone adopted it. | round 803, after fault C. | Take the uncertified head off (`Core::unwind_to`) and build on the QC block, as geth's leader proposes on the highest-QC block (`engine_v2/engine.go:3294-3319`; geth keeps its head as a side block, `core/blockchain.go:2419-2442`, xone executes on its head only). `d60fe5e` | soak |
| 12 | **Proposal fed before its block was readable** (the round-37 misses). | 6 occurrences, one caught with all three votes in the pool. | The producer reads the head from the history tip, not the store (`Core::import` commits before it pushes the history). `1676b87` | soak after 11:38 |
| 15 | **Sign block imported with its child was not signed** (from the xOneTS rehearsal, TS defect 15, issue #132). The producer signed only the head it saw in a tick; a sign block and its child imported within one tick (after a restart) got no sign transaction. | xOneTS soak: blocks 5280 and 6810. | On a new head, sign every sign block imported since the last head seen, within the 30-block freshness window (`miner/xdpos_worker.go:231-246`, `:1119`, `:1192-1209`); the first head after a start signs the tip only. `skipped_sign_blocks` | `crates/xone-node/tests/sign_blocks.rs` |
| 10 | Durability was a constant `Durable` passed to the machine and the unit. | code | the store's `durability()` before every signature (ADR-010 section 9), logged at start and on change. `b4e2ffe` | |

Diagnostics added: once per round the early return of `v2_tick` that fired (QC block, head, peer votes received); timeouts sent with the head, the votes received and the highest QC round; `sealed and sent (seal X ms, import Y ms)`; a BFT step whose persist-and-sign takes over 500 ms (`ea42d9a`).

## Builds

`b4e2ffe` (fixes 1-4, 9, 10), `6ea75ea` (5, 6), `3d385ef` (6, V1 fork, history check), `956767d` (7, 8), `ea42d9a` and
`c743c76` (diagnostics), `d60fe5e` (11), `1676b87` (12), `7202993` (merge of `origin/main` a3e71b4, #114 follow lag; both rule sets
kept in `chain.rs`). Each was built from this tree with `cargo build --release -j 6`; the devnet binaries were copied
next to the work directory so a rebuild never replaced a running one.

## The net2040 miss, reproduced (fix 1)

The same devnet, the same build except the BFT routing turned off (`chain.rs` hands nothing to the producer): the
producer never collected a vote and never had the QC of the head.

```
producer: round 2, head 33: the highest QC is for block 31 round 1, not the head 33 (no QC for the head yet); 0 peer votes received for round 1
producer: round 7, head 36: the highest QC is for block 35 round 6, not the head 36 (no QC for the head yet); 0 peer votes received for round 6
...
turns: {"rounds": 48, "eligible": 7, "sealed": 0, "missed": 7, "missed_rounds": [4, 8, 12, 16, 20, 24, 28]}  then epoch 1: 3 masternodes, xone penalised
```

With the routing on (the first build of `b4e2ffe`'s fixes): 27 of 27 eligible turns over rounds 1-115, then 34 of 34 over rounds 1-144 with all of `b4e2ffe`.

## 1. Turns: 600 V2 blocks with 4 validators (`956767d`, run on the nvme volume)

Blocks 31-631 (V2 rounds 1-601), from geth's chain data (`turns.py`):

| epochs | masternodes | rounds | eligible (xone leader) | sealed by xone | missed | timeouts (any leader) |
|---|---|---|---|---|---|---|
| 0-20 | 4 | 601 | 140 | 140 | **0** | **0** |

Every epoch: 7 eligible, 7 sealed, 0 timeouts (epoch 0 has 29 rounds, the last one 2). geth logged 0 bad or
invalid blocks (`grep -ciE "bad block|invalid block"` on the three geth logs: 0, 0, 0); 0 forensics proofs (the
only forensics lines are geth's `HighestCommittedQCs value not set` at the V2 switch, 09:50:51-55, before two
committed QCs exist). All five nodes at block 646 `0x92d72baa...` when the window closed. 95% upper bound on the miss
rate with 140 turns and 0 misses: about 3/140 = 2.1%.

## 2. Faults (`ea42d9a`, = `956767d` plus seal timing in the log)

Run 2026-11-12 10:11-10:23 (local time), blocks 632-884, V2 rounds 602-880. Record = xone's safety record
(`record.py`): highest voted round / vote / timeout / propose slot / lock round / highest QC / highest TC. Every field
of every record is at or above the one before it.

| Case | What was done | Record before | Record after | xone turns missed (cause) | Result |
|---|---|---|---|---|---|
| R | SIGTERM 6.5 s after an xone seal, start at once: it comes back as the leader of the next round (627) | 626/626/0/623/624/625/0 | 653/653/627/653/651/652/637 | 627, 633, 637 (no peers until the geth nodes redialled at 10:12:16, 35 s; `round 627, head 656: not synced: no peers`) | no block sealed without peers (before fix 5 this case sealed a block to nobody and stranded the node, run 4) |
| A1 | kill -9 inside xone's own round, in the seal-to-import window (`pending.rlp` present) | 653/653/627/653/651/652/637 | 681/681/657/679/679/680/667 | 657 (killed in it), 663, 667 (no peers until 10:13:38); then **683, 687** (see the round-37 investigation) | the record was still at propose 653: the block of round 657 was never signed; `the pending block 684 is dropped: the chain is at 691 without it`; no second digest |
| A2 | kill -9 2.5 s after an xone seal (inside another leader's round) | 681/681/657/679/679/680/667 | 723/723/695/723/721/722/705 | 697, 701, 705 (restart 8 s, peers back at 10:15:22) | resumed at propose 693, sealed 709 onwards |
| B | partition xone 60 s: every node `admin.removePeer` (xone has no static peer), then `admin.addPeer` | 723/723/695/723/721/722/705 | 784/784/724/783/782/783/743 | 727, 731, 735, 739, 743 (its turns inside the partition) | the network went on with 3 of 4 (TC for xone's rounds); xone timed out round 724 alone, on rejoin `imported 15 blocks, head 757` and sealed its next turn (761) |
| C | geth validator g1 stopped 60 s (10:18:37-10:19:37) | 784/784/724/783/782/783/743 | 833/833/802/833/831/832/804 | 803 (the sibling case below) | g1's rounds 785, 789, 793, 797, 801 timed out; xone signed the timeouts (slot 724 to 802) and formed the TCs (highest TC 743 to 804), and sealed its rounds after each TC (`block 800`, `803`, `806`, `809` sealed) |
| D | SIGSTOP xone 16 s across its round 843, then SIGCONT | 834/834/802/833/832/833/804 | 873/873/840/873/871/872/804 | 843 (stopped) | the others' TC moved the round; xone resumed and sealed its next turns (871, 877 ...) |

Safety, all cases: no height was sealed twice by xone with two hashes (197 `sealed and sent` lines, 0 heights with
two hashes); every record dominates the one before; geth logged no forensics proof and no equivocation (its only
forensics lines are `HighestCommittedQCs value not set`, at the V2 switch). Turns in the fault window: 65 eligible,
47 sealed, 18 missed: 15 to the injected faults and the devnet's 35 s redial, 1 to the missing sibling proposal
(fixed by 11), 2 to the head race (683, 687; fixed by 12).

## 3. Sign transactions (`956767d`, blocks 1-640)

42 sign blocks (every 15th): each has the sign transactions of all four masternodes. xone's sign transaction is in a
geth block 41 times of 42 (once in its own), geth's are in xone blocks 23 times of 126 (xone seals one block in
four); 125 of 168 land in block N+1, the other 43 in N+2 or N+3, all far inside the 30-block window of `isSigningTxStale`.
Before fix 7 (`3d385ef`, same blocks): xone's in geth blocks 1 of 42, geth's in xone blocks 0 of 126.

## 4. Durability

- Code: a node with `--mine` opens its store without group commit (`services/mod.rs`: `group_commit = producer_plan.is_none()`), so `xone_sync::mdbx` opens the environment with synced commits and every block commit is on the device before it returns (`Env::open_with(..., cfg.group_commit.is_none())`, `commit_block` S1-S7); `durability()` reports `Degraded(GroupCommit)` while a deferred commit exists. The producer now hands `durability()` to the safety unit and the BFT machine before every signature (fix 10) and logs it: every start in this rehearsal logged `store durability Durable`, and no change was logged.
- The safety record is written and synced before the signature exists (`unit.rs` `persist_then_sign`); the seal line carries the time: on the nvme volume 8-21 ms per seal (`sealed and sent (seal 18 ms, import 127 ms)`); on the scratchpad's root filesystem (99% full) an fsync stalled 80 s once (run 7), which is why the devnet moved.
- kill -9 in the seal-to-import window (fault A1, `kill-at-seal.py`: SIGKILL the moment `pending.rlp` appears): the record after the restart was at propose round 653 (the kill landed before the record write of the new round), so the block of `pending.rlp` was never signed; after the sync it was dropped (`the pending block 684 is dropped: the chain is at 691 without it`); no block of that round carries xone's signature, no second digest exists. In run 7 (before fix 8) the same kill also landed before the record write (record at 619 before and after); the restart, slowed 80 s by the host's fsync, signed the pending block then for the first time and sent it at once, beside a chain that had gone on: one signature for that round, but a stale block (fix 8). `crates/xone-builder/tests/producer_unit.rs` covers the retry with the same bytes.

## 5. Soak

Unattended 2026-11-12 10:22:58-12:31:16 (2 h 08 min) after the fault cases, scripted planned restarts every 15 min
(every third a kill -9), nothing else touched. Builds: `c743c76` from 10:23, `7202993` from 10:27 (planned restart),
`1676b87` from 11:38 (the soak's own planned restart took it).

| | |
|---|---|
| blocks / V2 rounds | 886-4541 (3,656 blocks) / 882-4568 (3,687 rounds) |
| epoch switches | 124 (4 masternodes in every epoch, xone in every list, never penalised) |
| restarts | 9: two binary swaps (SIGTERM), 7 scripted (5 SIGTERM, 2 kill -9) |
| eligible turns (xone leader) | 860 |
| sealed by xone | 829 |
| missed | 31: 30 inside the 9 restart windows (the node down, then without peers until the geth nodes redialled; 5 after each kill -9, which replays from the last state checkpoint for about 60 s), 1 the head race (round 1477, before `1676b87`) |
| timeouts | 31, all of xone's missed rounds; no other validator missed a round |
| on `1676b87`, outside restart windows | epochs 102-152: 352 eligible, 341 sealed, 11 missed, all in the three restart windows; 0 others (95% bound on the miss rate outside restarts about 3/341 = 0.9%) |
| seal time (record write + sync + signature) | 872 seals: median 19 ms, p99 27 ms, max 445 ms; no `slow safety write` |
| safety | 1,027 heights sealed by xone, 0 with two hashes; the record only rose (final: voted 4571, propose 4571, timeout 4337, lock 4569, highest QC 4570, highest TC 4347); 0 refusals; geth: 0 bad blocks, 0 forensics proofs |
| sign transactions | 243 sign blocks; xone signed 239 (the 4 others fell in restart windows), its transaction in a geth block 197 times; geth signs in xone blocks 344 times |
| end state | all five nodes at block 4541 `0xe88d1542...` |

One geth-side observation: from block 2190 one geth validator (g1) did not sign the sign blocks sealed by xone (20
blocks). After xone's restarts g1 did not redial it, so g1 received xone's blocks only through its batch syncer
(`xdcSyncer: imported batch`) and sent no sign transaction for them (not investigated in geth). Not an xone fault; with all peers connected (the 600-block window) every sign block had all four signers.

## Leader sibling (fault D and what geth does)

- Fault D (SIGSTOP of xone across its round 843, 16 s): geth timed out round 843 (TC), the next leader proposed on the
  block of the highest QC, which was the head (xone had proposed nothing in 843), so no sibling was needed; xone
  resumed and sealed its next turn (847). A timeout of a round with no block never needs a sibling.
- A sibling is needed when a round's block was imported but got no QC (its votes fell short) and the round timed out:
  geth's next leader takes the parent from the highest QC (`FindParentBlockToAssign`, `engine_v2/engine.go:3294-3319`;
  `Prepare` refuses any other parent, `:680-687`) and builds beside the uncertified head; geth keeps its own head
  until a child of the sibling arrives (`keepsV2Head`, `core/blockchain.go:2419-2442`). It happened once here, after
  fault C (round 801/803, block 810): geth reorged (`Chain reorg detected number=809 drop=1 add=2`), xone and the
  follower adopted the same branch (`adopted the branch of peer 5d340cd3: 1 block(s) of ours dropped, 2 imported`).
  xone was the leader of round 803 and did not build the sibling (it built on the head only); fix 11 (`d60fe5e`)
  makes it do so.
- Two rules of that path, found by the xOneTS Gate D fault case A1 (TB-12, issue #128) and mirrored in
  `producer.rs` (`behind_head`, `reinject`; tests `crates/xone-node/tests/sibling.rs`):
  1. **No sibling when the head's round is at or above ours.** The head's round proves a QC or TC of a round at or
     above ours; a quorum has left our round and votes only in its own (`engine_v2/vote.go:414-421`), so our sibling
     can never be certified. geth keeps its head (`core/blockchain.go:2413-2424`). Said once per round.
  2. **The taken-off block's transactions go back to the pool.** After `Core::unwind_to` the pool resets to the QC
     block, then every transaction of every dropped block is added again, oldest block first, as geth's pool reset
     re-injects the reorg diff (`core/txpool/legacypool/legacypool.go` `reset`, the `reinject` set). The pool's own
     rules drop what the new state refuses. The count is in the sibling log line.

## Restarts on this devnet cost turns

xone has no static peers here (so that fault B can isolate it), so after a restart it waits for the geth nodes to
redial it, about 35 s; the synced guard (fix 5) keeps it from sealing until then, and its turns in that window are
missed (cause: devnet peering, not the producer). On a public network xone dials its own peers.

## The round-37 investigation (fixed, `1676b87`)

Symptom: xone is the leader of round R; it stays in round R-1 for the full 10 s timeout although block R-1 was
imported and geth formed its QC within milliseconds; xone sends a timeout for R-1, the network times R out. Seen at
round 37 (an early fixed run, no restart, 3 peers), 263 (50 s after a restart), 683 and 687 (after the A1 restart),
and 1477 (soak, no restart nearby): about one head in 350.

Ruled out on the way: missing votes (`3 peer votes received for round 682`, three is the threshold of four), a
stalled producer thread (the round timer fired on time), a late block (imported 0.6 s after geth minted it), the gap
number (same formula as `engine_v2/vote.go:226-230`), peer bans, the host's fsync (seals at 8-21 ms on nvme).

Caught with the detail line of `c743c76` (soak, round 1477):

```
10:43:27 chain: imported 1 blocks, head 1474
10:43:35 producer: timeout round 1476 detail: held [0x608f:1474@0x7fa6c7/gap1430 0xED6F:1474@0x7fa6c7/gap1430 0xee8e:1474@0x7fa6c7/gap1430]; head 0x7fa6c7 epoch gap 1430 with 4 masternodes, candidates 4
10:43:35 producer: timeout for round 1476 sent (head 1474, 3 peer votes received for round 1476, highest QC round 1475)
```

Every input of the QC is in the machine's pool at the timeout, xone's own vote is not, and the commit that follows
every head (`committed block 1472`) is missing until the TC: the head's `Proposal` was not processed.

Root cause: the producer's tick read the head from `Core::head`, the store's head. `Core::import` commits a block to
the store and only then derives its history row and pushes it. A tick in between (the tick is 50 ms, the window a few
ms) saw the new head, fed `Proposal(head)` while `HistChain::header` could not read the block, `on_proposal` returned
on the missing header, and `fed` kept the block from being fed again; the three votes had arrived before the import
(geth formed its QC 50 ms after minting), each `form_qc` at a vote found no block, and nothing ran it after. Fix
(`1676b87`): the tick and the pending-block check take the head from the history tip, the only view the machine
reads. The soak ran this build from its 11:38 restart (numbers below).

## What blocks a borrowed-seat test on net2040

From this rehearsal (the owner runs the seat; these are the producer-side facts):

1. Fixes 1-8 and 11 are preconditions: without 1 a seat misses every turn (the net2040 abort of 0771fe10), without 2
   every block past Amsterdam is refused, without 5/6/8 a restart can leave the node on its own branch, without 7 the
   seat's sign transactions reach the chain only through its own blocks. They are on this branch (`7202993`).
2. **The quiet window is 0 on every chain but 50 and 51** (`ProducerService::new`: `SafetyUnit::open_or_init(store,
   signer, 0)`; `plan` refuses only 50 and 51): on net2040 the ADR-010 fence (`W = 2 * TimeoutPeriod * Base^MaxExponent`,
   120 s there) is not applied and no sighting of our own signature is fed to it. A second process with the same key
   is kept out only by the operator rule (stop the old signer, confirm 0 processes, then start). The mint-gate skill's
   pre-flight asks for a fence that works across hosts; it is not there.
3. A restart costs the turns until the node is peered and has proved itself caught up (fix 5); on the devnet that was
   35 s of geth redial. Restart a seat right after one of its seals.
4. A seat on a build before `1676b87` misses a turn whenever the head race (previous section) hits the round before its
   own: about one head in 350 here.
5. Branch adoption (fix 6) is refused while an account holds a balance above 2^256 (the history keeps those at the tip
   only); net2040 and Apothem were not checked for such accounts here.
6. `--miner.gaslimit` is not a flag: on a chain with the dynamic gas-limit row xone aims at geth's default 60M
   (fix 3). If the seat's geth was started with another `--miner.gaslimit`, xone moves the limit toward 60M by 1/1024
   per block (valid, but not the operator's setting).
7. Gates B and C are not recorded as closed for this build, which the mint-gate pre-flight requires.

## Reproduce

```
ops/devnet/gate-d.sh up   WORK XONE GETH      # chain + five nodes, meshed
ops/devnet/gate-d.sh turns WORK 31 631        # requirement 1
ops/devnet/gate-d.sh signs WORK 1 640         # requirement 3
ops/devnet/gate-d.sh sealkill|kill9|stop|start|cut|mesh WORK [x|g1]   # faults
ops/devnet/gate-d.sh down WORK; rm -rf WORK   # the keys are under WORK/chain
```
