# Proposal A3: governance and activation boundary for any PQ change

Issue [#19](https://github.com/XDCIndia/xOneRS/issues/19) (XA-14), track B research. **Research only.** This document describes the conditions under which a PQ rule could be activated. It does not schedule, recommend or enable anything. **No production activation without network governance, independent audits and a multi-client devnet.**

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`.

## 1. Why one client cannot do it

A PQ account or validator rule changes block validity. A node that does not implement it rejects the first block that uses it and forks off. XDC has several clients (geth-XDC as the oracle, and the clients in the fleet); the rebuild keeps the wire and consensus freeze of plan B10 (`../README.md`, no-activation rule 3). So activation is a network decision and a coordinated release, not a flag of one binary.

## 2. Gate Q (what must be true before activation)

From the no-activation rule in [`../README.md`](../README.md) and the `xone-rs-pq-readiness` skill. Each item is evidence, not a promise.

| # | Condition | Evidence required | State today |
| --- | --- | --- | --- |
| Q-1 | Written protocol specification | A spec in `spec/` with byte layouts, domains, fork rows, failure cases | none (this track is research) |
| Q-2 | Review by security and consensus owners | Signed review per proposal | not started |
| Q-3 | Independent audit | Report on the specification and on each client implementation | none; no library is audited (`../implementations.md`: "never independently audited") |
| Q-4 | Vectors that pass in at least two independent clients | Vector files from the oracle plus results in two clients | no vectors exist |
| Q-5 | Multi-client devnet run | Run record under `docs/perf.md` (blocks, restarts, rotation, mixed versions) | no devnet |
| Q-6 | Wallet and signer readiness | At least one wallet and one remote signer that can produce the signatures | none known (T24 unknown) |
| Q-7 | Cost lab under contract | Lab results on a host that meets `docs/perf.md` (`docs/pq/cost-lab-plan.md` (PR 281)) | one shared-host run only |
| Q-8 | Governance decision | A recorded decision by whatever body governs XDC (open question 1) | none |

## 3. Activation mechanics (if Q-1 to Q-8 pass)

1. **A fork row per network** in the generated fork table (`crates/xone-chainspec/generated/forks.json`, rendered in `docs/spec/fork-table.md`). The row exists as `Unscheduled` (the placeholder `99,999,999,999`, `spec/chainspec/unscheduled-sentinel.md`) from the day the code ships, so the code can be released before any height is set. Setting a real height is the activation act and a separate decision.
2. **Order of networks**: private devnet, then net2040 and the devnets (5551, 551), then Apothem (51), then mainnet (50). Each step needs the run record of the previous step. This follows the order the repository already uses for mint tests (`docs/ops/net2040-follow.md`, `docs/ops/apothem-follow.md`).
3. **Lead time**: a height set far enough ahead for every client to release and every operator to upgrade. No number is proposed; it is a governance parameter. NIST IR 8547 gives the outer dates for classical algorithms: ECDSA at 112 and 128 bits "Deprecated after 2030, Disallowed after 2035" (threat model section 2, [S5]).
4. **Mixed-version behaviour**: documented per surface (accounts, validators, transport). Nodes that have not upgraded fork off at the height; the release notes say so.
5. **No bypass**: no flag, feature or environment variable switches a PQ rule on or off in a production build (repository rule for every gate; the PQ lab is a separate non-shipping crate, `labs/pq/tests/no_ship.rs`).
6. **Rollback**: after activation, undoing it is a second governed fork. Before activation (while the row is `Unscheduled`) nothing needs undoing. The safety record of a validator (`../../adr/ADR-010-signer-safety.md`) is never restored from a backup that is behind it; the same holds for any registry or PQ key state a validator keeps locally.

## 4. Sequence of the proposals

| Order | Proposal | Why this order |
| --- | --- | --- |
| 1 (compatible, can start without the network) | Release and snapshot signing ([bridges-release-signing.md](bridges-release-signing.md)) | no consensus or wire change |
| 2 (wire only) | Transport hybrid key establishment ([transport-identity.md](transport-identity.md)) | no consensus change, but peers must be released together |
| 3 | Anchor and checkpoint authentication ([history-checkpoints.md](history-checkpoints.md)) | bootstrap trust must not depend on old ECDSA keys alone; part is compatible |
| 4 (consensus) | Validator keys and QC format ([validators-qc-tc.md](validators-qc-tc.md)) | changes header contents; needs the registry |
| 5 (consensus) | Accounts ([accounts-wallets.md](accounts-wallets.md)) | largest user impact; needs wallets |

## 5. Stop conditions

Any one of these stops a rehearsal or an activation and is filed as its own issue:

- two clients disagree on a state root or a QC verdict in a vector;
- a QC or a vote is accepted that has only a classical signature after the cut-over;
- a safety record or key state is found behind its last durable value after a restore;
- a library used in a client fails a NIST vector or a timing test (`docs/pq/cost-lab-plan.md` (PR 281) Q9, Q10).

## 6. What this document leaves to the owner

1. **Who governs.** The inputs do not name the body that decides a network rule change for XDC (masternode vote, foundation, a client-team process). The owner names it.
2. **Whether to take any proposal to governance**, and when. The issues say activation is outside this backlog.
3. **The Q-3 audit scope and who pays for it.**
4. **Which clients count as "independent" for Q-4** (the oracle line and its ports share semantics; ADR-002 lists the shared failure domains).
