# xOneRS rebuild planning documents

The [rebuild plan in issue #1](https://github.com/XDCIndia/xOne/issues/1) is the baseline proposal. The documents here add a review of correctness, durability, performance and post-quantum migration requirements. Research was completed on **10 November 2026**; repository packaging was added on **11 November 2026**.

| Document | Purpose |
| --- | --- |
| [Architecture review](reviews/2026-11-10-architecture-scale-pq-review.md) | Critique, primary-source project comparisons, language trade-offs, latency boundaries, PQ migration tracks, proposed gates and issue candidates. |
| [Source register](reviews/2026-11-10-architecture-scale-pq-sources.json) | Forty primary references with access dates and implementation pinning policy. |
| [Process](process/README.md) | PR rule (ponytail ladder), engineering-philosophy assessment and over-engineering audit. |
| [Claude master prompt](prompts/architecture-audit-and-issue-creation.md) | Explicitly invoked audit, evidence-backed issue creation, deduplication, bounded agent work and resumable sessions. |

## How to start

1. Read the review alongside issue #1 and the current repository instructions. Reconcile proposed changes with accepted ADRs; do not silently overwrite an existing decision.
2. When authorized to begin the audit and issue phase, copy the complete Claude master prompt into a session with repository access.
3. Establish exact source pins, inspect existing issues/PRs, and verify findings before publishing dedicated issues.
4. Finish with the reconciled issue ledger, gate dependency graph and next three to five ready work items. Implementation is a later scope.

## First outcomes to establish

- Wide-balance and revm compatibility across the complete rule and revert matrix.
- Crash-safe publication across the database and static files.
- Monotonic signer safety across rollback, stale restore and failover.
- Comparable performance measurements and operation-specific latency targets.
- A cryptographic inventory that separates compatible readiness work from coordinated protocol migration.

The existing [skills PR #2](https://github.com/XDCIndia/xOne/pull/2) is separate work. This documentation does not install those skills or imply that proposed Rust modules, harnesses or measurements already exist.
