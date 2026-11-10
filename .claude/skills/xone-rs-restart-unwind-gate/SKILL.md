---
name: xone-rs-restart-unwind-gate
description: The restart-safety and unwind-safety gate of the xOne Rust rebuild. Property-tests that unwind followed by re-execution gives the original state, and kill-tests the node at random points including mid-epoch, at an epoch switch, on a sibling head and inside the vote path. Use for Gate B (foundational correctness) and Gate D (isolated producer) of the gate DAG, when a change touches xone-db, unwind, reorg, set-head, the safety record or shutdown, or when the task says restart safety, kill -9, unwind, rollback, reorg, set-head, sibling block, "state undo".
---

# xone-rs-restart-unwind-gate

Clean genesis-to-tip runs do not find these bugs. reth-xdc left a sign-tx sender nonce partly reverted after an unwind. xOneGo lost the storage of an uncommitted sibling block, drifted its canonical index, and still has an open `SetHead` "state undo inconsistent" fault.

## Operating rules

1. Run on a follower datadir made for this test. Never on a producer, never on a datadir another run needs.
2. A wrong root after a restart or an unwind is a P0. There is no manual repair step in a passing run.
3. Everything one block changes (state, wide balances, receipts, canonical index, consensus snapshots) is in one write transaction. A test that finds two is a design defect, not a flake.
4. The safety record never goes back: after any kill, `highest_voted_round` on disk is greater than or equal to every vote that left the node. Unwind, `SetHead`, backup restore and snapshot import never lower it; the safety record is a separate store from chain state (see `xone-rs-durability-signer-gate`, which owns the boundary-by-boundary crash and signer tests).
5. Order: this gate and the durability gate pass before any public producer test. Random kill counts below supplement targeted boundary coverage; they do not prove every path.

## Inputs

- A synced follower datadir and its network.
- The offline executor and a vector range that contains `0x89` and `0x90` special transactions, a checkpoint, and an epoch switch.

## Steps

1. **Unwind property test** (`proptest`, in CI with a fixed seed and nightly with a new one). For random `N` and `k`:
   - execute to `N+k`; unwind `k` blocks; the full state hash and every consensus table equal the snapshot taken at `N`;
   - execute `k` blocks again; the result equals the first run at `N+k`.
   Force samples where the `k` blocks contain sign txs, a reward checkpoint, an epoch switch, and a wide-balance credit.
2. **Sibling test.** Import block `A` at height `h`, then its sibling `B`, then a child of `B`. The canonical index, header-by-hash for `A`, and the state root of `h+1` match geth. Restart and repeat the checks.
3. **Kill test (random, supplementary).** 100 runs of `kill -9` at a random time while following the tip. At least 10 in each class: mid-epoch, within 5 blocks of an epoch switch, during a snap or bootstrap, with a sibling as head. After each restart: the node reaches tip with no operator action, and `xone-rs-diff-exec` shows 0 mismatches for the 200 blocks after the restart.
4. **Vote-path kill test** (producer build on a local devnet only, never a public net; the targeted version is in `xone-rs-durability-signer-gate`). Kill between "record written" and "vote sent", and between "vote sent" and "next round". After restart the node never signs a second, different vote or timeout for a round it already signed.
5. **Bounded shutdown.** `SIGTERM` returns within 5 s in 20 of 20 runs.
6. **Disk-full test.** Fill the test volume. The node stops importing with one clear alarm line, does not corrupt the datadir, and resumes after space is freed.

## Gates

- Unwind property: 0 failures over at least 10,000 cases.
- Sibling test: equal to geth before and after restart.
- Kill test: 100 of 100 recover; 0 wrong roots; 0 manual repairs. Supplements, not replaces, the targeted boundary kills.
- Vote path: 0 double signs.
- Shutdown: 20 of 20 within 5 s.

## Done

The results, the seeds and the commit are on the Gate-B or Gate-D work item. Any failure has a divergence record and is in the conformance loop.
