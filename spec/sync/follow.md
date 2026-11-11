# Live follow at the tip

## Rule

`Follower::step(chain, peers, now)` is a pure driver over a `PeerSource`: no clock, no I/O, no threads. One step:

1. **Pushes.** `NewBlock` blocks go to a queue of 16 (ADR-007 Q4, drop oldest: a dropped block is a gap and is fetched by request); `NewBlockHashes` entries go to a queue of 64. An announced hash that the chain does not hold is fetched (`GetBlockHeaders` by hash, one; then `GetBlockBodies`).
2. **Import.** Queued blocks are inserted in number order with `Chain::insert`; the outcome is counted (`imported`), a block the chain refuses is counted (`rejected`) and never retried from the same answer.
3. **Gap fill.** A block whose parent is unknown and whose number is above head+1: the missing blocks `head+1..number-1` are requested by number (`GetBlockHeaders`, at most 128 per request: ADR-007 Q9 and geth `MaxBlockFetch`, below the 192 that legacy peers accept; then `GetBlockBodies` for the headers returned) and inserted, then the block. If they do not connect to our head the block is on another branch and its ancestors are walked by hash (at most 32) to a block we hold; fork choice then decides (`spec/chain/fork-choice.md`).
4. **Tip poll.** For each peer in rank order whose head hash we do not hold: show it (`GetBlockHeaders` by hash, checked to hash to the claimed head). A peer that cannot show its head is skipped and the next one is asked. The first head above ours starts a gap fill from `head+1`, preferring that peer.
5. **Peer order.** Peers are tried in descending TD. TD only orders (geth `rankedPeers`). A peer that serves nothing, a wrong chain (numbers out of order, a repeated header, a first header that does not follow our head), or a block the chain refuses, is skipped for that block and the next peer is asked (geth tries candidates in descending TD and skips peers already proved empty, `fullSyncOnce`). If no peer serves the block, `Report::stalled_at` names it: a typed result, not a wait.
6. **Caught up is hash equality.** Some peer's head hash equals our head hash, and our head is within one epoch of the highest block heard of (geth `peersConfirmAtTip`, which also guards against a lagging peer that sits at our height). TD is never read for this. A peer with the highest TD in the world and our head hash is not ahead; a peer with TD 0 and a head we do not hold is followed.
7. **At the tip** is caught up, or a fresh head: the head is at most `at_tip_freshness_secs` old, for three steps in a row (geth `tipCatchAtPeerTicks >= 3`). The window is 30 s, or on a V2 chain `TimeoutPeriod * max(TimeoutSyncThreshold, 1) + MinePeriod` when larger (mainnet: 60 * 3 + 2 = 182 s), taken from the chain spec's highest-round V2 config.
8. **Answers are untrusted.** The driver checks linkage and that headers hash as claimed; `Chain::insert` checks the body against the header and everything else. Request ids (eth/101) and the matching of a reply to its request belong to the `PeerSource`; the driver never rewinds bulk progress because a gap fill always starts at `head+1` of the moment.

## Source

- `eth/sync_xdc.go:556` `newXDCSyncer`; `:112` (`xdcForceSyncCycle` 500 ms); `:563-589` `atTipFreshnessWindow`; `:737` (`tipAtPeerTolerance` 3), `:756-776` head freshness and the three-tick confirmation; `:1759-1769` `peersConfirmAtTip` (hash equality plus within one epoch of the best known tip); `:1781-1840` `maybeTransitionToAtTip`; `:2574-2586` retry in descending TD, skipping peers already proved empty; `:4297` `fetchHeaderByHash`; `:5298-5330` `fetchHeaderBatch` (cap 192, response must start at the requested number); `:6314-6376` `bestPeer`, `rankedPeers` (descending `LatestBlock`, which on XDPoS2 is the handshake TD).
- Recreate guide 11 (`docs/port/recreate-guide/README.md`): TD is not a block number.
- ADR-007 Q3, Q4 (`docs/adr/ADR-007-operations-limits.md` section 2).
- Pinned xOneGo reference named by the issue supplement: `pkg/eth/xdpos2_sync.go:57-118,306-324,380-441`, `xdpos2_bodyfetch.go:3-24`, `xdpos2_blockprop.go:126-151` (read as lessons, not ported).

## Networks

All.

## Rust

`xone_sync::follow::{Follower, PeerSource, PeerInfo, Push, Report, Status, at_tip_freshness_secs}`. The binding to `xone-net` sessions is `conformance/sync/src/lib.rs` (`NetSource`): xone-sync and xone-net are the same layer and may not depend on each other, so the wire adapter lives with the live test.

## Vectors

`crates/xone-sync/tests/follow.rs` (fake peer set): pushed blocks, announcements, a 25-block gap in two requests, a lying-TD peer (highest TD, empty history, unknown head) that does not stall the follower, a high-TD peer at our head that is not ahead, a TD-0 peer that is ahead and is followed, a typed stall with bounded requests, reordered, duplicated and partial answers, a sibling branch adopted by fork choice, the Q4 bound, the freshness window and the three-tick rule. `conformance/sync/tests/live.rs` (ignored): a mining loopback geth-XDC (pin `6fc8e8c2`) over real RLPx sessions, eth/101 framing and the real executor; head lag at most 2 (`conformance/sync/run-live.sh`). Result of the last run: 110 blocks followed, 98 by catch-up (gap fill) and 12 pushed live, head hash equal to geth's last announced block, 456 lag samples with maximum 0, `at_tip` and `head_fresh` true; the run before followed 50 blocks (48 + 2 live) with 2,880 samples at maximum 0. The pinned geth mines in bursts of 20 to 25 blocks (see the script), so a single run follows one burst live.

## Not covered

- The 24 h public follow on net2040 and Apothem (head lag p99 at most 2): owner-approved, read-only hosts.
- 50 blocks pushed live in one run: the loopback geth mines 12 to 21 blocks per run while a peer is connected. The live test runs the real executor with `TIPTRC21Fee` unscheduled in the devnet spec (the executor has no TRC21 path, XB-08); the blocks hold no transactions and every root is still compared with the header.
- A peer's head is the head of its Status (`Event::Connected::head`, added to `xone-net` for this driver), then what it announces: eth/100 and eth/101 carry no live head, as geth notes (`eth/sync_xdc.go:4219-4232`).
- BFT message budgets under delayed import (supplement 3) belong to XB-15; this driver only keeps pushed blocks distinct and pull-recoverable.
