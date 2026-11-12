# xOne execution plan

This plan describes the source reviewed at `876bb8e9679d3df67aabccea32cfe7047a3284c3`. [#51](https://github.com/XDCIndia/xOne/issues/51) holds the build backlog and final gates. [#57](https://github.com/XDCIndia/xOne/issues/57) defines readiness for individual deliverables. The filename is retained for existing links; the first skeleton batch is historical.

## Goal

Deliver an XDC-compatible client that can initialize, follow, restart and serve its supported operator interfaces from a qualified artifact. Acceptance requires execution on the client's own state, exact consensus validation, durable canonical and signer state, interoperable sync, measured resource behavior and the declared network soaks. Merged modules and short diagnostic runs are progress toward those gates.

Keep Rust for the core and Go for the pinned oracle/exporter. Keep post-quantum readiness and protocol research separate from current-consensus implementation. Neither a language choice nor a microbenchmark establishes quantum resistance or network finality latency.

## Current evidence

PR #93 merged the development follower, sync, observability, snap, V2 replay and BFT slices. Its author reports local tests and a short private-loopback follow. Hosted CI at the integration head failed; the reported billing cause has not been independently established by this source review.

[RUNS.md](../../conformance/harness/RUNS.md) records a 432,000-block Apothem genesis replay and separate diagnostic pivot windows. The V2 pivot window covers 302 epoch switches; the 1,000-switch and full-history gates remain open. Archive-sourced pivots do not establish authenticated bootstrap. Node/sync contexts still lack the V2 gap-snapshot wiring required at checkpoint blocks, and `FileStore` does not persist staged-sync rows.

The [current review snapshot](../../.audit/current-review.json) is a source-audit record. It is not a worker lease, test result or gate-completion ledger. Preserve historical run records and add exact provenance for new results.

## Readiness and ownership

Start a deliverable when its own prerequisite outputs have the required evidence. A parent issue can stay open while a dependent foundation slice proceeds. For example, storage design can enable a writer implementation while the storage umbrella still awaits real I/O and unwind proof. Keep every final parent acceptance criterion.

Before a worker changes code, record its deliverable, responsible session, branch/worktree, base and head, owned paths, prerequisite evidence, pending writes and next action. Reconcile current remote state before claiming work. One integrator owns shared manifests, lockfiles and branch integration. Preserve another session's branches and datadirs.

Use these states distinctly: proposed, implemented, reported tested, independently tested, merged and qualified. A merged PR does not automatically advance an umbrella to qualified. [#57](https://github.com/XDCIndia/xOne/issues/57) still owns the machine-readable deliverable graph, readiness validation and active-session ledger; this document does not claim those tools exist.

## Next deliverables

Work on independent slices in parallel only after ownership and interface boundaries are clear. Start import coherence and datadir ownership first; run independent CI guard work in parallel. Serialize edits that share node-service or RPC files.

| Order | Deliverable | Evidence needed before integration |
| --- | --- | --- |
| Parallel | [#101 CI guards](https://github.com/XDCIndia/xOne/issues/101) ([PR #102](https://github.com/XDCIndia/xOne/pull/102)), [#6 oracle drift](https://github.com/XDCIndia/xOne/issues/6) and [#57 readiness](https://github.com/XDCIndia/xOne/issues/57) | Scan errors and real-crate violations fail the assurance runner; missing oracle access cannot appear qualified; seeded drift fails; hosted CI state is separate from local checks |
| 1 | [#95 Import coherence](https://github.com/XDCIndia/xOne/issues/95) | Fallible consensus-context work cannot leave durable head and visible history split; post-commit failure and restart regressions pass |
| 2 | [#97 Exclusive datadir ownership](https://github.com/XDCIndia/xOne/issues/97) | A second opener is refused before recovery, truncation, key-file changes or publication; the ownership guard covers the required process lifetime |
| 3 | [#94 Empty transaction rejection](https://github.com/XDCIndia/xOne/issues/94) | Malformed wire input reaches a typed rejection before size calculation; no import-task panic |
| 4 | [#96 Discovery clock](https://github.com/XDCIndia/xOne/issues/96) | The runtime uses the correct absolute-expiry clock and completes a controlled exchange with an independently clocked peer |
| 5 | [#99 Log-query bounds](https://github.com/XDCIndia/xOne/issues/99) and [#100 RPC state views](https://github.com/XDCIndia/xOne/issues/100) | Maximum-u64 ranges cannot bypass work limits; queries retain a consistent state view or return a typed availability error |
| 6 | [#98 Sync confirmation](https://github.com/XDCIndia/xOne/issues/98) | Confirmation is bound to peer/session/head and expires on disconnect; an echo-only high-ranked peer cannot prevent progress |

These regressions feed the existing execution, storage, consensus and operator umbrellas. After them, complete the actual staged-sync wiring and persistence, V2 checkpoint contexts, transport policy in [#60](https://github.com/XDCIndia/xOne/issues/60), supported operator surfaces and remaining interoperability. Keep full replay, real storage-fault testing, isolated producer proof, exact packaged-artifact conformance and public qualification. Refresh the next batch when a decisive result changes readiness.

## Workflow

1. Read the current tracker, review snapshot and active execution state. Claim one ready deliverable on nonoverlapping paths.
2. Keep the declared source, oracle and fixture pins fixed. Establish a failing regression or measured baseline. Re-pin only through an explicit reviewed change and rerun the affected evidence.
3. Implement one bounded change against the agreed interface. Use Sonnet for implementation and substantive review, and Haiku for bounded extraction or fixture bookkeeping, only when those models are actually available. Record the provider used; do not claim an unavailable route ran.
4. Run targeted checks and the relevant differential, negative-input, fault, interoperability or performance gate. Record the exact command, source tree, artifact/input digests, environment, result and remaining parent criteria.
5. Have an independent verifier repeat the decisive check on a clean checkout with the same inputs.
6. Open one PR with the required repository identity/trailers, specification link, evidence and limits. Integrate under the existing review and merge authority.
7. Update the deliverable and parent evidence. Close the parent only when all its acceptance criteria pass.

## Verification loop

Select ready output -> establish failure/baseline -> implement -> targeted checks -> decisive gate -> independent verification -> PR/integration -> checkpoint -> select the next ready output.

A code, specification, fixture or dependency change invalidates the evidence it affects. Run full historical replay at declared integration boundaries and relevant pin changes. Do not rewrite expected output, skip validation or reduce a gate to obtain a pass. Three failures of an unchanged approach require a written root-cause decision and alternatives. A root, durability or signing fault stops the affected qualification path; unrelated work can continue.

Checkpoint after each decisive result and external write. On a rate limit, retain Retry-After/reset metadata, reduce concurrency and use bounded backoff. Reconcile an uncertain mutation by its stable issue/PR identity before retry. Do not claim automatic resumption without an actual scheduler.

## Final qualification

Keep Gate A feasibility, Gate B foundational correctness, Gate C sync/resources and follower qualification, Gate D isolated producer safety, and Gate E packaged release/public rollout. Evidence for a prerequisite deliverable permits dependent engineering work; it does not bypass a final gate.

Before public producer qualification, complete the required B/C/D evidence, exact-artifact checks and the authorized host/run decisions in [#47](https://github.com/XDCIndia/xOne/issues/47). Keep the mainnet follower's full own-state replay and 14-day soak in [#48](https://github.com/XDCIndia/xOne/issues/48). Mainnet signing and protocol activation remain separate decisions. Track Q research cannot substitute for those client gates.
