# Transition runbook: xOneGo to xOne (Rust)

Issue [#49](https://github.com/XDCIndia/xOneRS/issues/49) (XB-29). This is a runbook. **It performs nothing.** Every action on a validator seat, a fleet host, a public network, or a branch or issue of `XDCIndia/xOneGo` is an owner action; each is marked **[owner]**. The agent steps are read-only analysis and writing this file.

Facts were read on **2026-11-14**. xOneGo: `master` `d1f667fd` (tag `v1.4.7`), `main` `c992b0c2` (tag `v1.4.8`). xOne: `origin/main` `a99fcf7290fc9a78be30a700badb7504a931be2a`. geth-XDC oracle `6fc8e8c2088b7f18264f5663036669d04fa7becf`. Sources: `git` on a blobless bare clone of `https://github.com/XDCIndia/xOneGo.git`, `gh pr list` and `gh issue list` on that repository, issues #47, #48, #49, #50 of this repository, `docs/adr/ADR-010-signer-safety.md`, `docs/ops/keys.md`, `docs/ops/first-mint-local.md`. The numbers below go stale; section 9 gives the commands that recompute them.

## 0. Owner decisions needed (one line each)

1. **When to move the Apothem validator `0x68a2`** (blocked by Gate E, issues #47 and #48; section 5).
2. **How long xOne must hold the seat with no incident before xOneGo is archived** (the issue leaves the period to the owner; section 6 proposes a rule, not a number).
3. **Which xOneGo branches to merge, close or delete** (the table in section 8 is a proposal; the owner approves each action).
4. **Whether the local sweep branch and any other local-only work is pushed to xOneGo** (section 3: the three local-only SHAs named in the issue are not in any clone on this host).
5. **Whether to fix the `main`/`master` release split now** (section 2: tag `v1.4.8` is on `main` and not on `master`).
6. **The SkyNet identity xOne uses on Apothem** (section 5, step 11).

## 1. Wedge-fix-only policy for xOneGo

Text for the xOneGo `README.md` and for the tracker. Proposed; the owner adopts it.

> **xOneGo is in maintenance.** The Go client is replaced by the Rust client xOne (`XDCIndia/xOneRS`). From the date of this notice xOneGo accepts only **wedge fixes**: a change that is needed to keep an existing xOneGo node syncing, importing blocks or producing on a network it already runs on (Apothem validator `0x68a2`; net2040 where it is seated). A wedge is a stall, a divergence from the canonical chain, a double-sign risk, or a node that cannot restart. No features, no refactors, no new RPC methods, no dependency bumps except a security advisory that affects a running node.
>
> Every wedge fix lands on `master` by a pull request that says `Closes #<issue>` in the first line of the body, with the failing block height or the incident date, and a test or a replay that fails before and passes after. A fix that is only on `main` or on a branch is not shipped.

Rules that follow:

| Rule | Why | Evidence |
| --- | --- | --- |
| `Closes #n` goes in a PR that merges into **`master`** (the default branch). A merge into `main` does not close the issue | PR #752 merged into `main` on 2026-11-12 with `Closes #624, Closes #623`; #624 closed, #623 is still open | `gh pr view 752`, `gh issue view 623` in `XDCIndia/xOneGo` |
| One keyword per issue: `Closes #a, Closes #b`, never `fixes #a, #b, #c` | PR #683 says `fixes #680, #681, #682`; #680 closed and #681 and #682 are still open although the fix is on `master` (`c4d38475`) | `gh pr view 683`, `gh issue view 681 682` |
| A release is a merge of `main` into `master` followed by the tag on `master` | issue #50 section 1 records the model ("`main` for work, `master` for releases; every v1.4.x tag is a merge PR from `main`"); `v1.4.7` is `master` (`d1f667fd`, the merge of PR #751 from `main`); `v1.4.8` was tagged on the merge of PR #752 into `main` and is not an ancestor of `master` | `git merge-base --is-ancestor v1.4.8 master` is false at the date above |
| Lessons that are not wedge fixes become issues in `XDCIndia/xOneRS`, not xOneGo branches | the rebuild keeps lessons as rules, not as ports (issue #50, #49 "Non-goals: porting xOneGo code") | issue #50 |

Exceptions are decided by the owner in the issue itself.

## 2. State of xOneGo on 2026-11-14

| Item | Value | Source |
| --- | --- | --- |
| Default branch | `master`; not archived | `gh api repos/XDCIndia/xOneGo` |
| `master` | `d1f667fd`, tag `v1.4.7` | `git describe master` |
| `main` | `c992b0c2`, tag `v1.4.8`; `main` has 2 commits that `master` lacks and `master` has 2 that `main` lacks | `git rev-list --count master..main`, `main..master` |
| Latest GitHub release | `v1.4.8`, 2026-11-12 | `gh api repos/XDCIndia/xOneGo/releases` |
| Branches | 150 including `main` and `master`: of the other 148, 72 are fully merged into `master`, 1 is merged into `main` only, 75 are ahead of both | `git for-each-ref refs/heads`, `git rev-list --count master..<b>` |
| Open issues | 327 (352 open items including 25 open pull requests) | `gh issue list`, `gh api repos/XDCIndia/xOneGo` |
| Pull requests | 167 in total: 115 merged, 25 open | `gh pr list --state all` |

The issue text named `master` `39d112b3` = v1.4.6. It has moved: `master` is now `d1f667fd` = v1.4.7 and the latest release tag is on `main`.

## 3. The SHAs named in the issue

| SHA | What it is | State on 2026-11-14 |
| --- | --- | --- |
| `39ea9b05` | V1 gas fees to the recovered proposer | Not on `master`. On branches `fix/62-v1-fee-recipient-recovered-proposer` (PR #691 open), `fix/62-v1-checkpoint-smc-state-fallback` (#692 open), `test/62-netv12-block-replay` (#695 open) |
| `7278ae91` | Credit fees to the ecrecovered signer | Not on `master`. On `fix/credit-fees-to-ecrecovered-signer` (PR #717 open) |
| `2367e797` | Keep the miner tick off the cold gap state | **Now on `master`** (it was not when the issue was written) |
| `c0f707d5` | Restarted full or archive node no longer replays the cold gap state | Not on `master`. On `fix/xone-seat-cold-state` only; no PR |
| `d2f1d742`, `00a8d70f`, `20710617` | Local-only commits (GitHub returned 422 on 2026-11-11) | **Not found** in `/mnt/data/workspace/xOneGo`, `xOneGo-616`, `xOneGo-730`, `xOneGo-restart`, `xOneGo-worktrees/*` or `/root/workspace/xOneGo` by `git cat-file`. The local sweep branch `fix/apothem-85220370-v146` is also not in any of them. Unless an operator's `reflog` or another host holds them, treat them as lost |

The three V1 fee-recipient commits are already a rule in xOne (`spec/evm/fee-recipient.md`); Apothem has been V2 since block 56,828,700 (`crates/xone-chainspec/generated/forks.json`), so the V1 fee rule is not an Apothem seat wedge. That is why section 8 proposes closing those branches.

## 4. Issues

Closed since the issue was written: #730 (closed 2026-11-11) and #733 (closed 2026-11-11).

Open although a fix is merged (found by reading the 115 merged pull requests for keywords and issue numbers, then checking issue state):

| Issue | Fix | Why it is still open | Proposed action [owner] |
| --- | --- | --- | --- |
| #623 | PR #752, merge `c992b0c2` on `main`, `Closes #623, Closes #624` | merged into `main`, not the default branch | Close with the SHA, or leave open until `main` is merged into `master` |
| #681, #682 | PR #683, merge `c4d38475` on `master`, `fixes #680, #681, #682` | only the first number after the keyword closes | Close each with the SHA |
| #688 | PRs #689 (`6776c3b5`) and #690 (`695d3d0a`) on `master` reference it | the PR text says "(#688)" without a closing keyword; whether the issue's symptom (proposals never reach a QC) is fixed is not verified here | Owner or a replay confirms, then close with the SHA, or keep open with the reason |

Not found by this method: open issues whose fix is on `master` without a PR reference (the 327 open issues were not each read). The recompute command in section 9 lists candidates; a human decides each. The open list also contains feature requests (for example #534, #535, #536, #537, #543 and #556 by title) that the wedge-only policy would close as "not accepted"; they were not counted.

## 5. Apothem validator seat move (`0x68a2`)

Preconditions (all must be true; each is evidence in a tracker, not a feeling):

| # | Precondition | Where |
| --- | --- | --- |
| P1 | Gate E, net2040 part: 3 seals on a borrowed seat and the soak minimum (the issue proposes 200 eligible turns, 72 h, 3 epoch changes, 5 restarts), 0 conflicting signatures, every miss explained | issue #47 (XB-27) |
| P2 | Gate E, Apothem part with an owner-assigned validator, same soak | issue #47 |
| P3 | Mainnet follower parity from genesis and a 14-day soak. Issue #49 lists #48 as a blocker for this transition; whether it is needed for a testnet seat is an owner call | issue #48 (XB-28) |
| P4 | A rollback rehearsal (seat back to the previous client) executed once and timed | issue #47 |
| P5 | The signer safety record exists and survives restore and failover (ADR-010 sections 3, 6, 8) | `docs/adr/ADR-010-signer-safety.md`, `crates/xone-consensus/src/v2/safety.rs` |
| P6 | Owner approval for this specific run recorded in the issue before the run | issue #47 |

Today (2026-11-14, from the hourly tracker on issue #140, 04:30Z): Rust nodes on net2040 hold seat `0xa0ea` since 2026-11-12 with 0 misses, and the Rust archive node passed 3/3 on 2026-11-12. P1 and P2 are not recorded as passed in #47 (the issue is open). `--mine` on Apothem (chain 51) is allowed by PR #142 (merged); mainnet (50) stays refused.

Procedure, as the procedure of issue #47 and ADR-010. Every step is **[owner]** unless marked.

1. **Choose the target.** An xOne host with a healthy disk and 8 or more peers on Apothem, running as a follower of chain 51 and at the tip (lag at most 2 blocks). A host that already holds a seated validator is not used for tests.
2. **Pre-flight, [agent] can prepare and the owner runs:** `xone` at a tagged release (`docs/ops/release.md`), `--network apothem`, the keystore file and password file in mode 0600 (`docs/ops/keys.md`), no key on the command line, the safety directory outside every backup set (ADR-010 section 4).
3. **Peer in first.** Confirm 8 or more peers. A dead seat held for about 300 s once nearly stalled net2040 (issue #47); the rule is peer-in first and abort within 45 to 60 s.
4. **Stop the old signer.** Stop the xOneGo process that holds `0x68a2`. Do not start xOne with the same key while xOneGo can still sign.
5. **Confirm the old signer is gone.** The process is gone, the port is closed, and no block, vote or timeout signed by `0x68a2` appears in the quiet window below. This is the double-sign guard.
6. **Fence.** Start xOne with the key. At start the fence is closed (no signature of any kind). During the conservative quiet window `W` it observes the network; any message signed by our key that this process did not emit is checked against the record, and the record is raised above it. When `W` passes with no unexplained sighting the fence opens (ADR-010 section 8).
7. **First slot.** Watch the first slot assigned to `0x68a2`. If it is missed, **abort within 45 to 60 seconds**: stop xOne, start xOneGo again with the same key only after confirming xOne is gone (steps 4 and 5 in reverse).
8. **Verify seals.** By timestamp, child link and the client's own log (not by the board alone). Three consecutive seals is the smoke; the soak minimums of #47 are the gate.
9. **Hold.** The new seat holder runs the soak by eligible turns. Every miss is recorded with its cause. With n eligible turns and 0 misses the report states the 95 percent upper bound on the miss rate (about 3/n), not 100 percent.
10. **Rollback is rehearsed, not improvised.** Same stop, confirm, fence rule, in the other direction. The rehearsal is timed once (P4).
11. **SkyNet identity.** xOne reports to the board as `xone-rs-<network>-<host>` (the form used by `docs/ops/apothem-follow.md`, `xone-rs-apothem-h95`). The identity xOneGo uses for `0x68a2` on the board was not read for this document. The owner states the new name and whether the xOneGo entry is removed or left to age out.
12. **Record.** The seat move, the times, the SHA of the xOne build and the first verified seals go in the issue.

The seat moves themselves are owner actions. This document does not stage a key, start a node or touch a fleet host.

## 6. Retirement of xOneGo

Do these in order, each after the previous one is recorded. All **[owner]**.

1. **No-incident period.** xOne holds `0x68a2` with no incident for a period the owner sets. Proposed rule for the owner to fill in: a minimum number of eligible turns (the Gate E soak) **and** a minimum elapsed time **and** at least one epoch change and one restart of the xOne process inside it. An incident is a missed slot not explained by the network, a double-sign sighting, or a state root mismatch.
2. **Freeze.** Add the section 1 text to the xOneGo README. Stop accepting feature pull requests. Land or close every open pull request per section 8.
3. **Reconcile.** Close each issue that has a fix on `master`, with the SHA; keep the rest open with a reason or close them as "superseded by xOne" with a link to the xOne issue that carries the lesson.
4. **Last release.** Merge `main` into `master`, tag the final xOneGo release on `master`, and keep the release assets. Do not delete tags.
5. **Branches.** Delete the 72 branches already merged into `master` and the stale branches of section 8. Keep nothing that holds the only copy of a lesson that is not yet an xOne issue.
6. **Archive.** Archive `XDCIndia/xOneGo` on GitHub. An archived repository is read-only and its history stays.
7. **Record the decision** in issue #49 and in `docs/transition.md` (this file).

## 7. Risks and stop conditions

| Risk | Control |
| --- | --- |
| Two signers for one seat (double sign) | Steps 4 to 6 of section 5; ADR-010 section 8 quiet window; never start the second signer before the first is confirmed gone |
| A seat held dead while xOne is not producing | Abort within 45 to 60 s (step 7) |
| A wedge in xOneGo during the maintenance period | Wedge fixes only, to `master`, with a replay (section 1); the Apothem seat stays on xOneGo until section 5 completes |
| Lost local work | The three local-only SHAs are not found (section 3); an operator checks other hosts before the freeze |
| A fix is on `main` only | Section 1 rule and section 8 row for `fix/623-624-selfmined-syncinfo` |
| Retiring xOneGo before xOne is proven | Section 6 step 1 gate and P1 to P4 |

Stop and file an issue in `XDCIndia/xOneRS` on: a wrong state root, a conflicting signature, data loss, or a safety record found behind its last durable value.

## 8. Branch reconciliation (proposal)

The 75 branches that are ahead of both `master` and `main` (and one that `main` has), with a proposed disposition. "Lesson" means: close the branch and keep the idea as a rule in xOne (the spec page or file named); no xOneGo code is ported. The owner approves every action. "Stale" rows marked "PR merged" were checked by `git cherry` against `master`; the counts are in the row.

Counts by proposed class (76 branches):

- Promote: 1
- Wedge fix: 5
- Wedge fix (net2040): 1
- Review: 16
- Lesson: 18
- Stale: 35

| Branch | Commits ahead of `master` | Last commit | Pull request | Proposed disposition [owner] | Reason and evidence |
| --- | --- | --- | --- | --- | --- |
| `fix/623-624-selfmined-syncinfo` | 1 | 2026-11-12 | #752 merged | merge main to master in the next release PR, then delete the branch | PR #752 merged to `main` with Closes #623, Closes #624; `master` is the default branch so #623 stayed open. |
| `fix/215-bounded-resume` | 1 | 2026-07-09 | #259 open | rebase, then merge if the resume cost still reproduces; else close | PR #259 open since 2026-07-09. |
| `fix/260-fullexec-forkresolve` | 3 | 2026-07-09 | #261 open | rebase, then merge if still reproducing; else close | SetHead fallback to the canonical index; PR #261 open since 2026-07-09. |
| `fix/706-bounded-shutdown` | 5 | 2026-07-28 | #709 open | review and merge to master (wedge fix) | shutdown wedge #706 and #703 (SIGTERM deadlock); PR #709 open. |
| `fix/xone-restart-head0` | 1 | 2026-11-07 | none | review and merge to master (wedge fix) | rebuilds the resume header index with ordered scans; restart `headBlock=0` incidents 2026-11-05 and 2026-11-09 (#50 timeline). |
| `fix/xone-seat-cold-state` | 3 | 2026-11-07 | none | review and merge to master (wedge fix); c0f707d5 is on this branch only | restarted full or archive node no longer replays the cold gap state; no PR yet. |
| `fix/net2040-penalty-sticky-531555` | 1 | 2026-11-04 | #727 open | merge only if xOneGo stays on net2040; else close and keep as lesson (spec/consensus/penalties-and-comeback.md) | V2 sticky-penalty parity at net2040 531,555; PR #727 targets `main`. |
| `feat/62-ethstats-report-coinbase-and-role` | 2 | 2026-07-26 | #694 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #694 open since 2026-07-26; 4 months behind master (rebase needed). |
| `feat/net5151-skip-v1-difficulty` | 1 | 2026-07-21 | #677 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #677 open since 2026-07-21; 4 months behind master (rebase needed). |
| `feat/xdpos-consensus-metrics` | 1 | 2026-07-08 | #244 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #244 open since 2026-07-08; 4 months behind master (rebase needed). |
| `fix/209-header-request-timeout` | 1 | 2026-07-07 | #211 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #211 open since 2026-07-07; 4 months behind master (rebase needed). |
| `fix/225-snap-pivot-trigger` | 2 | 2026-07-08 | #226 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #226 open since 2026-07-08; 4 months behind master (rebase needed). |
| `fix/228-wire-connlimiter` | 1 | 2026-07-08 | #254 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #254 open since 2026-07-08; 4 months behind master (rebase needed). |
| `fix/234-real-tracers` | 1 | 2026-07-08 | #257 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #257 open since 2026-07-08; 4 months behind master (rebase needed). |
| `fix/239-peer-scoring-bans` | 1 | 2026-07-08 | #256 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #256 open since 2026-07-08; 4 months behind master (rebase needed). |
| `fix/262-stale-target-refresh` | 4 | 2026-07-09 | #263 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #263 open since 2026-07-09; 4 months behind master (rebase needed). |
| `fix/264-leader-selection` | 5 | 2026-07-09 | #265 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #265 open since 2026-07-09; 4 months behind master (rebase needed). |
| `fix/264-throughput` | 6 | 2026-07-09 | #266 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #266 open since 2026-07-09; 4 months behind master (rebase needed). |
| `fix/268-v2-nonce` | 7 | 2026-07-09 | #270 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #270 open since 2026-07-09; 4 months behind master (rebase needed). |
| `fix/275-v1-epoch-noconsecutive` | 1 | 2026-07-10 | #276 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #276 open since 2026-07-10; 4 months behind master (rebase needed). |
| `fix/318-hardening` | 6 | 2026-07-14 | #320 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #320 open since 2026-07-14; 4 months behind master (rebase needed). |
| `test/62-netv12-block-replay` | 6 | 2026-07-27 | #695 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #695 open since 2026-07-27; 4 months behind master (rebase needed). |
| `test/675-676-gap-provenance-replay-harness` | 1 | 2026-07-27 | #702 open | owner: merge only if it still reproduces on master and is a wedge fix; else close | PR #702 open since 2026-07-27; 4 months behind master (rebase needed). |
| `auto-loop/1-01-harden-qc-v2` | 6 | 2026-07-04 | #113 closed | close; lesson: single-masternode QC hardening (spec/consensus/v2-certificates.md) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/1-02-engine-recover` | 7 | 2026-07-04 | #115 closed | close; lesson: engine goroutine panic recovery | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/1-03-cap-pooled-txs` | 8 | 2026-07-05 | #117 closed | close; lesson: cap GetPooledTransactions response | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/2-01-chainid-replay-guard` | 6 | 2026-07-05 | #119 closed | close; lesson: chain-id replay guard at pool admission (spec/txpool) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/2-02-cap-inbound-txs` | 7 | 2026-07-05 | #121 closed | close; lesson: cap inbound transactions per message | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/2-03-cap-snap-lookups` | 8 | 2026-07-05 | #123 closed | close; lesson: bound snap lookups on misses (spec/snap/messages.md bounds) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/3-01-eth-peerlock-announce` | 7 | 2026-07-05 | #125 closed | close; lesson: peer lock held across tx announce | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/3-02-rpc-ratelimiter-bound` | 8 | 2026-07-05 | #128 closed | close; lesson: rate-limiter map bound (RPC) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/3-03-p2p-accept-backoff` | 9 | 2026-07-05 | #130 closed | close; lesson: accept-loop backoff (xone-net) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `auto-loop/4-01-rlpx-neg-length` | 8 | 2026-07-05 | #132 closed | close; lesson: negative-length RLP in RLPx handshake (xone-net) | stacked auto-loop branch from 2026-07-04/05; PR closed. |
| `feat/miner-keyfile` | 1 | 2026-07-27 | #704 open | close; xOne never takes a key or password in argv or env (docs/ops/keys.md) | PR #704 open. |
| `fix/232-eip2-low-s` | 1 | 2026-07-08 | #246 open | close; lesson: high-S question Q4 in docs/pq/threat-model.md section 9 | PR #246 open. |
| `fix/62-v1-checkpoint-smc-state-fallback` | 2 | 2026-07-25 | #692 open | close with its base (#691); lesson: V1 checkpoint list from 0x88 state (spec/consensus/v1-checkpoint-masternodes.md) | stacked on #691. |
| `fix/62-v1-fee-recipient-recovered-proposer` | 1 | 2026-07-25 | #691 open | close; lesson already in spec/evm/fee-recipient.md (commit 39ea9b05) | V1 fee to the recovered proposer; Apothem has been V2 since block 56,828,700 so the V1 rule is not a seat wedge. |
| `fix/credit-fees-to-ecrecovered-signer` | 1 | 2026-08-01 | #717 open | close; lesson already in spec/evm/fee-recipient.md (commit 7278ae91) | same fee-recipient class as 39ea9b05; PR #717 open. |
| `fix/native-robustness-107-96-94` | 1 | 2026-07-05 | none | close; lesson: historical re-exec must not share the producer processor (spec/chain/insert.md) | no PR; 1 commit 2026-07-05. |
| `fix/xdc-detection-from-genesis-config` | 1 | 2026-07-31 | #714 closed | close; lesson: custom chains by genesis (issue #711 stays with the rebuild: spec/chainspec/custom-genesis.md) | PR #714 closed; issue #711 open. |
| `fix/xdpos-0x89-signing-parity` | 1 | 2026-07-12 | #310 closed | close; lesson: 0x89 signing model in spec/evm/special-transactions.md | PR #310 closed. |
| `auto-loop/1` | 5 | 2026-07-04 | none | delete the branch | auto-loop loop scaffolding (loop.sh, prompts), no PR. |
| `auto-loop/2` | 5 | 2026-07-05 | none | delete the branch | auto-loop loop scaffolding (loop.sh, prompts), no PR. |
| `auto-loop/3` | 6 | 2026-07-05 | none | delete the branch | auto-loop loop scaffolding (loop.sh, prompts), no PR. |
| `auto-loop/4` | 7 | 2026-07-05 | none | delete the branch | auto-loop loop scaffolding (loop.sh, prompts), no PR. |
| `dependabot/npm_and_yarn/site/next-15.5.18` | 1 | 2026-05-12 | none | close (dependabot bump of /site, 2026-05-12) | no PR. |
| `feat/109-pathscheme` | 3 | 2026-07-06 | #174 merged | confirm the content is on `master` (PR merged; 3 commit(s) not patch-equal, probably squashed), then delete the branch | The pull request merged into `master`. `git cherry master`: 3 commit(s) without a patch-equivalent, 0 with. |
| `feat/160-txpool-gossip` | 1 | 2026-07-06 | #163 merged | delete the branch (its PR #163 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/166-readmission` | 1 | 2026-07-06 | #169 merged | delete the branch (its PR #169 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/171-replay-verify` | 1 | 2026-07-07 | #186 merged | delete the branch (its PR #186 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/185-preverify-bench` | 1 | 2026-07-07 | #207 merged | delete the branch (its PR #207 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/19-rpc-parity` | 1 | 2026-07-06 | #173 merged | delete the branch (its PR #173 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/21-v28-wiring` | 1 | 2026-07-06 | #172 merged | delete the branch (its PR #172 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/60-7-finish` | 1 | 2026-07-06 | #162 merged | delete the branch (its PR #162 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `feat/75-parallel-verify` | 2 | 2026-07-07 | #184 merged | confirm the content is on `master` (PR merged; 2 commit(s) not patch-equal, probably squashed), then delete the branch | The pull request merged into `master`. `git cherry master`: 2 commit(s) without a patch-equivalent, 0 with. |
| `feat/add-more-scripts` | 5 | 2026-02-24 | none | delete the branch | pre-XDC ETH2030 work by other authors (2026-02/03), no PR. |
| `feat/retype-naa-tx-type` | 13 | 2026-03-27 | none | delete the branch | pre-XDC ETH2030 work by other authors (2026-02/03), no PR. |
| `fix/165-eftest-build` | 1 | 2026-07-06 | #176 merged | delete the branch (its PR #176 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/167-race` | 1 | 2026-07-06 | #168 merged | delete the branch (its PR #168 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/170-gap-pruning` | 1 | 2026-07-06 | #177 merged | delete the branch (its PR #177 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/181-xdcx-gas` | 1 | 2026-07-06 | #182 merged | delete the branch (its PR #182 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/220-fastpath-reorg` | 1 | 2026-07-08 | #222 merged | delete the branch (its PR #222 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/277-headqc-recovery` | 7 | 2026-07-10 | #281 closed | close and delete the branch (PR #281 was closed) | PR closed without merge. |
| `fix/277-v2-epoch-halt` | 2 | 2026-07-10 | #279 closed | close and delete the branch (PR #279 was closed) | PR closed without merge. |
| `fix/318-syncinfo-round-catchup` | 4 | 2026-07-14 | #319 closed | close and delete the branch (PR #319 was closed) | PR closed without merge. |
| `fix/devnet-dev` | 4 | 2026-02-25 | none | delete the branch | pre-XDC ETH2030 work by other authors (2026-02/03), no PR. |
| `fix/ethstats-client-version` | 1 | 2026-07-07 | #214 merged | delete the branch (its PR #214 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/fullsync-throughput` | 1 | 2026-07-07 | #204 merged | delete the branch (its PR #204 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `fix/rename-eth2030-to-xone` | 1 | 2026-07-12 | #296 closed, #293 merged | delete the branch (its PR #293 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `measure/net5050-tx-encoding-parity` | 1 | 2026-07-07 | #217 merged | delete the branch (its PR #217 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `plan/binary-tree-vm-changes` | 2 | 2026-03-02 | none | delete the branch | pre-XDC ETH2030 work by other authors (2026-02/03), no PR. |
| `rebrand/187-xonego` | 1 | 2026-07-07 | #210 merged | confirm the content is on `master` (PR merged; `git cherry` timed out), then delete the branch | The pull request merged into `master`. `git cherry`: not completed. |
| `release/v1.1.0` | 1 | 2026-07-07 | #208 merged | delete the branch (its PR #208 merged to master) | The pull request merged into `master`. `git cherry master`: all 1 commit(s) have a patch-equivalent on `master`. |
| `vyom` | 7 | 2026-07-05 | none | delete the branch | loop.sh scaffolding copy, no PR. |
| `xdc/network-presets` | 1 | 2026-07-02 | #37 closed | close and delete the branch (PR #37 was closed) | PR closed without merge. |
| `xone/net2040-validator-ready` | 1 | 2026-11-06 | #728 closed | close and delete the branch (PR #728 was closed) | PR closed without merge. |

The 72 branches that are fully merged into `master` need no decision beyond deletion; the list is `git branch -r --merged master` on the bare clone (section 9). `dependabot/*` and the pre-XDC ETH2030 branches (`feat/add-more-scripts`, `feat/retype-naa-tx-type`, `fix/devnet-dev`, `plan/binary-tree-vm-changes`) are by other authors and 2026-02 to 2026-05.

## 9. Recompute commands

```sh
git clone --bare --filter=blob:none https://github.com/XDCIndia/xOneGo.git xg && cd xg
git describe --tags --abbrev=0 master; git rev-parse --short master main
git merge-base --is-ancestor v1.4.8 master; echo $?               # 1: v1.4.8 is not on master
for b in $(git for-each-ref refs/heads --format='%(refname:short)' | grep -v '^master$\|^main$'); do
  echo "$(git rev-list --count master..$b) $(git rev-list --count main..$b) $b"; done    # ahead of master, ahead of main
git branch --contains <sha>                                         # where a commit lives
git cherry master <branch>                                          # '-' = patch-equivalent already on master
gh pr list -R XDCIndia/xOneGo --state all --limit 1000 --json number,headRefName,state,baseRefName
gh issue list -R XDCIndia/xOneGo --state open --limit 400 --json number,title
# open issues named by a merged PR (keyword or number), then check each by hand:
gh pr list -R XDCIndia/xOneGo --state merged --limit 500 --json number,title,body,baseRefName
```

## 10. Not covered

- The 327 open issues were not each read. Only those named by a merged pull request were checked.
- Content equivalence of branches was checked with `git cherry` for the merged-PR branches only; the other dispositions rest on the PR state, the date and the commit subject, not on a diff review.
- The xOneGo node's SkyNet name, the host that runs `0x68a2`, and whether other operators run xOneGo were not read.
- No fleet host, key, node or network was touched.
