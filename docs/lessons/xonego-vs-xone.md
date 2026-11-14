# xOneGo history against the xOne rebuild: lessons and carry-forward rules

Issue [#50](https://github.com/XDCIndia/xOneRS/issues/50) (XB-01). This document turns the comparison of the issue into a repository document and checks each lesson against what is on `main` today. It adds no rule that is not in the issue; it adds the **status of each rule in the code** and the facts that changed since the issue was written.

Pins. xOneGo (`XDCIndia/xOneGo`): `master` `d1f667fd` (tag `v1.4.7`), `main` `c992b0c2` (tag `v1.4.8`), read 2026-11-14. xOne: `origin/main` `a99fcf7290fc9a78be30a700badb7504a931be2a`. geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`. The issue's pins were xOneGo `39d112b3` (v1.4.6) and xOne `2e77a4cf`.

Evidence tags. **in main**: a file or test on `origin/main` implements or checks it (read, not re-run, unless a row says "run"). **partly**: some of it is there and the row says what is missing. **not yet**: no artifact found. The xOneGo SHAs are lessons only; no Go code is ported.

Sources: the tables of issue #50 (sections 1 to 6: build classification, timeline, bug classes C1 to C16, approach deltas, keep and drop, rules CF-1 to CF-13); `git log` and `git merge-base` on a bare clone of xOneGo; `gh pr list` and `gh issue view` on xOneGo; the xOne files named in each row.

## 1. What changed since the issue was written

| Item in issue #50 | State on 2026-11-14 | Source |
| --- | --- | --- |
| xOneGo `master` = v1.4.6 `39d112b3` | `master` is `d1f667fd` = v1.4.7. The latest tag `v1.4.8` (2026-11-12) is on `main` (`c992b0c2`) and is **not** an ancestor of `master` | `git describe master`; `git merge-base --is-ancestor v1.4.8 master` is false |
| "XDCIndia/xOneGo#730 open although PR 731 merged" (open item 3) | #730 closed 2026-11-11 | `gh issue view 730 -R XDCIndia/xOneGo` |
| "XDCIndia/xOneGo#733 open (server stub)" | #733 closed 2026-11-11 ("implement snap state backend") | `gh issue view 733` |
| `2367e797` (miner tick off cold gap state): "branch only" | now on `master` | `git merge-base --is-ancestor 2367e797 master` is true |
| `39ea9b05`, `7278ae91`, `c0f707d5`: "not on master" | still not on `master` (branches `fix/62-v1-fee-recipient-recovered-proposer`, `fix/credit-fees-to-ecrecovered-signer`, `fix/xone-seat-cold-state`) | `git branch --contains` |
| `d2f1d742`, `00a8d70f`, `20710617`: "local clone only" (open item 1) | not found in any xOneGo clone or worktree on this host | `git cat-file -e` over `/mnt/data/workspace/xOneGo*`, `/root/workspace/xOneGo` |
| "Every v1.4.x tag is a merge PR from `main`" | `v1.4.8` is the merge of PR #752 **into `main`**; the `Closes #623` in it did not close #623 because `master` is the default branch | `gh pr view 752`; `gh issue view 623` (open) |
| Rust clients | all four xOne clients mint on net2040 (Rust seat `0xa0ea` since 2026-11-12, TS own seat `0xfC71`, Python `0xc33a`, C++ `0x00ce` per the tracker on issue #140) | the hourly comment of 2026-11-14 04:30Z on issue #140 |

The runbook that acts on the open items is [`docs/transition.md`](../transition.md) (issue #49).

## 2. Bug classes and what prevents them now

For each class of issue #50 section 3: the artifact on `main` and its status.

| # | Class | Prevention in the repository | Status |
| --- | --- | --- | --- |
| C1 | EVM semantics | `crates/xone-evm` on revm `=43.0.3`; rule pages `spec/evm/*.md`; oracle vectors `conformance/vectors/evm/`; `crates/xone-evm/tests/vectors.rs`, `wide_matrix.rs`; proof `docs/assurance/wide-balance-proof.md` (PR for #7). Sequential differential replay: `conformance/sweep` | in main (EEST corpus not checked here) |
| C2 | Fork-rule coupling | one generated table `crates/xone-chainspec/generated/forks.json` from `conformance/exporter`; pages `spec/chainspec/*.md`; test `crates/xone-evm/tests/no_literal_chain_id.rs` | in main |
| C3 | Side branches and siblings | `spec/chain/insert.md`, `spec/chain/fork-choice.md`, `spec/state/changesets.md`; tests `crates/xone-state/tests/siblings.rs`, `crates/xone-node/tests/sibling.rs`, `crates/xone-sync/tests/fork_choice.rs`, `crates/xone-builder/tests/reorg.rs` | in main |
| C4 | Non-canonical data in consensus | `spec/consensus/rewards-v1.md` ("blocks come from the parent chain of the checkpoint block, by hash, never from a store keyed by height (xOneGo `8be7d783`)"); `crates/xone-consensus/tests/reward_vectors.rs`, `reward_mutants.rs` | in main |
| C5 | Restart and recovery | `spec/chain/resume.md` (resume a consistent head or a typed error); `docs/adr/ADR-005-storage-and-publication.md`; tests `crates/xone-sync/tests/kill.rs`, `crates/xone-node/tests/restart.rs`; fault plan `tests/fault/PLAN.md` | in main |
| C6 | Lock coupling and deadlock | `spec/chain/insert.md` (header verification before the import lock; no consensus read under it); `crates/xone-sync/tests/locks.rs`; `ci/check-globals.sh` | in main |
| C7 | Producer liveness on cold state | `spec/producer/timing.md`; `crates/xone-builder/tests/producer_sim.rs`, `producer_schedule.rs`; rehearsal `docs/ops/gate-d-rehearsal.md` | in main (a measured budget on a public network is Gate E, issue #47, open) |
| C8 | Fee recipient and routing | `spec/evm/fee-recipient.md`; `crates/xone-evm/tests/special_fee_floor.rs`; vectors `conformance/vectors/special/fee_recipient.jsonl` | in main |
| C9 | Sign txs and penalty comeback | `spec/evm/special-transactions.md`, `spec/txpool/special-txs.md`, `spec/consensus/penalties-and-comeback.md`; `crates/xone-node/tests/sign_blocks.rs`; vectors `conformance/vectors/special/sign_blocks.jsonl` | in main |
| C10 | BFT safety and liveness | `spec/consensus/v2-bft-vote-path.md`; `docs/adr/ADR-010-signer-safety.md` (write-ahead record, fence); `crates/xone-consensus/tests/bft_machine.rs`, `bft_mutants.rs`; `tests/signer/PLAN.md` | in main |
| C11 | Penalty and epoch-switch parity | `spec/consensus/v2-masternodes-and-leader.md`, `v1-penalties.md`, `v2-epoch-switch-and-gap-snapshot.md`; `crates/xone-consensus/tests/penalty_vectors.rs`, `v1_mainnet_penalised.rs` | in main |
| C12 | Sync scale and completeness | `spec/sync/stages.md`, `spec/snap/*.md`, `spec/retention.md`; `crates/xone-snap`, `crates/xone-node/tests/minimal.rs`; storage decision issue #135 | in main (mainnet follower parity and the 14-day soak are issue #48, open) |
| C13 | Runtime archive dependency | `spec/bootstrap-anchor.md` (one pin from outside, everything else verified); `crates/xone-node/src/services/gaprpc.rs` reads the 0x88 pool of a gap block from an archive RPC (`--snap.gap-rpc`) but treats the source as untrusted and verifies every proof against a header root it holds | **partly**: no trust in the source, but a snap sync of a geth-XDC-fed node can still need that RPC to be reachable (see CF-9) |
| C14 | Identity and version | one build stamp checked by `ci/release.sh` (refuses `unknown` and `-dirty`); `crates/xone-node/tests/live/run.sh` checks `web3_clientVersion` equals `xone --version`; `docs/ops/release.md` | in main (that `--version` never boots a node was not checked here) |
| C15 | Operations | `docs/ops/*.md`, `spec/ops/{ethstats,heartbeat,logs,metrics}.md`, `docs/ops/release.md`, `docs/ops/keys.md` | in main |
| C16 | Process | `docs/process/README.md` (PR rule); the transition runbook `docs/transition.md` (branch and issue rules for xOneGo) | partly: the "one long-lived branch, `Closes #n`" rule for xOne is in the release skill and in each issue, but not in `docs/process/README.md` |

## 3. Carry-forward rules and what enforces them

| Rule | Statement (issue #50) | Enforcement on `main` | Status |
| --- | --- | --- | --- |
| CF-1 | Every incident becomes a named regression test with archive prestate and a mutant before the fix merges | mutant suites `crates/xone-builder/tests/mutants.rs`, `producer_mutants.rs`, `crates/xone-consensus/tests/{bft,reward,v2}_mutants.rs`, `mutants.rs`; `conformance/negative-gen`; sweep cases `conformance/vectors/special/cases/` | in main; no CI rule yet says "a fix PR without a mutant fails" |
| CF-2 | No rule switch outside the generated fork table; no chain-ID literal in execution or consensus code | `crates/xone-evm/tests/no_literal_chain_id.rs`; table `crates/xone-chainspec/generated/forks.json` | in main for `xone-evm`; the two consensus-side chain-id branches C3 and C4 of `spec/chainspec/non-parameter-switches.md` have no table row yet (gap G3 there) |
| CF-3 | Side blocks execute on their own persisted parent; lookup by hash covers side branches | `spec/chain/insert.md`, `spec/state/changesets.md`; `crates/xone-state/tests/siblings.rs` | in main |
| CF-4 | Consensus inputs come from canonical, accepted blocks only, in ascending order; missing data is a typed error, never empty | `spec/consensus/rewards-v1.md`; typed errors in `xone_sync::bootstrap::AnchorError` and the chain `Error`; `spec/chain/resume.md` | in main |
| CF-5 | A restart resumes a consistent head or refuses to start; never head 0 on a non-empty datadir | `spec/chain/resume.md`; `crates/xone-node/tests/restart.rs`; ADR-005 kill points | in main |
| CF-6 | No lock is held while consensus reads state; consensus is a step machine with Persist before Send | `spec/chain/insert.md`; `crates/xone-sync/tests/locks.rs`; `docs/adr/ADR-010-signer-safety.md` section 6 | in main |
| CF-7 | The producer tick never waits on cold state; the budget is measured | `spec/producer/timing.md`; `docs/ops/gate-d-rehearsal.md` | partly: measured in a rehearsal, not on a public network |
| CF-8 | Sign txs are part of the producer contract, also while penalized | `spec/txpool/special-txs.md`, `spec/evm/special-transactions.md`; `crates/xone-node/tests/sign_blocks.rs` | in main |
| CF-9 | No runtime dependency on an archive or a trusted RPC | `spec/bootstrap-anchor.md`; `gaprpc.rs` (verified, untrusted source) | **partly**: see C13. Decision for the owner below |
| CF-10 | One version source for every identity surface; `--version` never boots a node | `ci/release.sh` stamp check; `crates/xone-node/tests/live/run.sh` | partly: the second clause was not checked |
| CF-11 | One long-lived branch; fixes merge or close within 14 days; PRs use `Closes #n` | `main` is the only long-lived branch of this repository (the others are short-lived feature branches); `docs/transition.md` (PR for #49) states the rule for xOneGo | partly: no automated check of the 14 days or of the keyword |
| CF-12 | Every audit finding on the reference client is mapped and tested before a public producer run | issue #44 (XB-24) with a planned date in `docs/plans/2026-11-11-execution-plan.md` (task b44) | not yet |
| CF-13 | No public test before Gates B, C and D; owner approval per run; abort within 60 s on a missed borrowed slot | issue #47; `docs/ops/gate-d-rehearsal.md`; ADR-010 section 8 | in main as a procedure; the gate state lives in the issues |

## 4. Approach deltas confirmed by merged code

| Dimension | xOneGo | xOne on `main` | Evidence |
| --- | --- | --- | --- |
| EVM | own | revm `=43.0.3` with a journal wrapper for wide balances | `crates/xone-evm/Cargo.toml:15`, `crates/xone-evm/src/journal.rs`, ADR-002 |
| Fork rules | hand-written per call site | generated typed table | `crates/xone-chainspec/generated/forks.json`, `docs/spec/fork-table.md` (PR for #6) |
| State | flat, head only, gap replay | MDBX flat state plus changesets; any retained block opens without replay | `spec/state/changesets.md`, `spec/retention.md` |
| Consensus shape | engine reads state under the import lock | verification before the import lock; Persist before Send | `spec/chain/insert.md`, ADR-010 |
| Storage | convention | durability contract with kill points | ADR-005, `tests/fault/PLAN.md` |
| Sync | full sync, then anchored snap; server stub | staged sync, snap/1 both ways, anchored minimal mode | `spec/sync/stages.md`, `spec/snap/*.md`, `spec/bootstrap-anchor.md` |
| Repository | 850k lines incl. roadmap packages | 14 crates, XDC only | `crates/` |

## 5. Keep and drop, status

**Keep** (issue #50 section 5), with the artifact that carries it:

| Kept idea | Artifact | Status |
| --- | --- | --- |
| Era-stratified real-transaction sweep as a diagnostic, never a gate | `conformance/sweep` | in main |
| One regression test per incident, replayed from archive prestate | `conformance/vectors/special/cases/`, `conformance/vectors/replay/` | in main |
| Bounded caches for gap-block state validated against the header root | `spec/consensus/v2-epoch-switch-and-gap-snapshot.md` | in main |
| Persisted proposer double-mint guard inside the safety record | `docs/adr/ADR-010-signer-safety.md` | in main |
| Anchored minimal mode with fail-closed verification and the gap-pool provenance rule | `spec/bootstrap-anchor.md`, `spec/bootstrap/minimal-mode.md` | in main |
| Bounded shutdown and borrowing snap from another peer | `spec/sync/follow.md`, `spec/snap/pivot.md` (not read line by line for these two) | not verified |

**Drop** (own EVM, state and trie; head-only state and gap replay; one insert lock across verify and execute; height-keyed pre-canonical records; side blocks as cached state; headers in memory; archive as runtime dependency; roadmap packages; two long-lived branches; fixes left on branches; `Refs` instead of `Closes`): each item is the opposite of an artifact in section 2 except two. "Archive as runtime dependency" is **partly** dropped (C13). "`Refs` instead of `Closes`" is a process rule, not code (C16).

## 6. The four open items of the issue

| # | Item | State on 2026-11-14 |
| --- | --- | --- |
| 1 | The EVM-sweep branch exists only in a local clone | The three SHAs and `fix/apothem-85220370-v146` were not found in any clone on this host. Unless another host holds them, they are lost; the xOne sweep (`conformance/sweep`) is the replacement |
| 2 | Four fix branches not on xOneGo `master` | `2367e797` is now on `master`; `39ea9b05`, `7278ae91`, `c0f707d5` are not. Proposed disposition per branch: `docs/transition.md` section 8 |
| 3 | #730 open although its fix merged | closed 2026-11-11 |
| 4 | The restart `headBlock=0` hang has no root-cause commit | not found. `fix/xone-restart-head0` ("rebuild resume header index with ordered scans", 2026-11-07, no PR) is the nearest branch and is not on `master`; whether it is the root cause of the 2026-11-05 incident is not established by any record read |

## 7. Decisions for the owner

1. Accept the keep and drop lists of section 5, or record changes in a comment on issue #50.
2. **CF-9:** accept `--snap.gap-rpc` as a verified, untrusted, optional source (the node never trusts it), or require that no sync path depends on an archive RPC.
3. Add a CI rule for CF-1 ("a fix PR must add a mutant") and CF-11 (`Closes` keyword), or leave them as review rules.
4. Say whether open item 4 needs a root-cause investigation or is retired with xOneGo.

## 8. How to refresh

```sh
git clone --bare --filter=blob:none https://github.com/XDCIndia/xOneGo.git xg && cd xg
git describe --tags --abbrev=0 master; git merge-base --is-ancestor v1.4.8 master; echo $?
git branch --contains <sha>            # is a fix on master, main or only a branch
gh issue view <n> -R XDCIndia/xOneGo --json state
```

Update section 1 and section 6 for each new xOneGo tag until issue #49 retires xOneGo.
