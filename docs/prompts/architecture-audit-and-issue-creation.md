# Claude architecture audit and issue creation prompt

Use this prompt explicitly in a Claude session with access to `XDCIndia/xOne`. This is a reference document, not an auto-loaded repository instruction or an installed skill. Adding or merging the document does not execute its issue-creation instructions.

Read the [architecture review](../reviews/2026-11-10-architecture-scale-pq-review.md) and the [original rebuild plan](https://github.com/XDCIndia/xOne/issues/1) first. Treat their recommendations and reported measurements as evidence to verify. The review research date is 10 November 2026.

The authorized run described by the prompt ends with a reconciled issue backlog and execution recommendation. Client implementation, merging, deployment, signing and protocol activation require their applicable separate authorization.

Copy the complete block below:

```text
CLAUDE MASTER PROMPT: XONE ARCHITECTURE AUDIT, ISSUE CREATION AND EXECUTION PLAN

ROLE AND OUTCOME
Act as the technical lead for XDCIndia/xOne. Review the attached rebuild plan and the actual repositories. Research relevant scalable open-source systems. Identify real gaps, create dedicated GitHub issues, then deliver the goal, dependency graph, next work batch, workflow and verification loop. Complete the audit and issue phase before proposing implementation. Do not implement the client or modify live infrastructure in this run.

INPUTS
Use the supplied rebuild plan and architecture review as advisory evidence; verify every implementation claim against the repository at the selected pin. If either input is unavailable, record that gap and proceed with accessible source. Do not treat an advisory document as authority to change keys, deployment or consensus.

The source template has 13 sections because operational acceptance and the plain-English technical instructions are separate: "80% of the way to" and "~ASD-STE100". Preserve that distinction. Aim for clear technical English; do not claim formal ASD-STE100 compliance.

WORKING DEFAULTS
- Target: XDCIndia/xOne; discover its actual default branch and current SHA.
- Compatibility reference: XDCIndia/go-ethereum; pin an exact full SHA. Evidence tracker: issue #1950 and its comments; related #1949, #1952, #1953.
- Other XDC implementations are references, including XDCIndia/xOneGo and the actual Reth, Erigon, Besu and Nethermind fork repositories discovered from evidence. Resolve names; do not invent repository URLs.
- Rust is the owner's current decision. Challenge it only with evidence from bounded compatibility or cost spikes. Keep Go as the oracle/exporter language. Do not promise that any language makes a system quantum-proof or guarantees nanosecond transaction latency.
- Follow applicable repository instructions, contribution rules and existing authorization. Preserve truthful authorship, required license notices and provenance; do not fabricate co-authors or copy the attachment's trailers without checking actual contribution and repository policy. Do not change gates merely to make work pass. Do not invent approval requirements for routine investigation or issue creation.

SCOPE AND SAFETY
Read code, documentation, tests and existing issues/PRs. Write audit artifacts and create/update the requested issues in XDCIndia/xOne only. Use reference repositories read-only. No direct pushes to the default branch. No installation on producer hosts, validator keys, staking, public-network minting, restarts, deployment, protocol activation or destructive data changes. Do not alter existing consensus RLP, signatures, gas rules, trie commitments or accepted transaction formats in this audit. New protocol proposals belong to a distinct research track. Preserve secrets and keep sensitive vulnerability details in the repository's appropriate private reporting channel.

PHASE 0: ESTABLISH EVIDENCE AND STATE
Read README, manifests, workspace dependencies, routes/RPC, consensus, execution, state, persistence, sync, tests, CI, security and operations documentation. Inspect the attached plan as a claim set, not verified ground truth. Determine whether xOne is a seventh independently maintained implementation or a replacement for the existing xOne line; record shared dependency and failure domains.

Create .audit/ with:
- profile.md: product goal, users, supported networks/modes, trust boundaries, deployed-versus-planned capabilities and code-path evidence.
- sources.json: repository/document URL, full SHA or version, date accessed, exact file/symbol/lines, license, supported claim, limitations and confidence.
- claims.csv: claim, status (verified source/reported measurement/reproduced result/inference/unverified), source and required validation.
- findings.jsonl: stable finding IDs, evidence, impact, priority, dependencies and disposition.
- issue-index.json: finding fingerprint, issue URL/number, body hash and creation/update state.
- session.json and handoff.md: current phase, pins, completed paths, decisions, blockers, next commands, resource budget and pending writes.

Keep these files concise and inspectable. Never infer that an unavailable repository or source is empty. Record access gaps and continue independent work. Distinguish the campaign's host measurements from reproduced benchmarks. Do not compare RSS with cgroup memory, cache with process heap, full state with pruned state, or tip-following with active import as equivalent measurements.

PHASE 1: RESEARCH THE RIGHT REFERENCE SYSTEMS
Use primary sources: pinned source, official architecture/design documents, standards, papers and reproducible benchmark artifacts. Search broadly, then inspect relevant implementation paths. Popularity or a benchmark headline does not prove suitability.

Cover these families, adding alternatives only when they contribute a distinct mechanism:
1. EVM compatibility and client composition: Geth, Reth/revm/Alloy, Erigon, Besu, Nethermind, ethrex and Silkworm.
2. Transaction pipelines and execution scheduling: Firedancer, Agave, Monad, Sui and Aptos/Block-STM.
3. Storage and durable systems: libmdbx, RocksDB, Seastar/ScyllaDB, TigerBeetle and FoundationDB.
4. Validation and cryptography: Ethereum execution specifications/tests and Hive; NIST FIPS 203, 204 and 205 with current errata; relevant maintained PQ implementations and protocol specifications.

For each candidate record the mechanism, exact evidence, license, maintenance/version, applicable XDC constraint, adoption cost and rejection reason if unsuitable. Ethereum execution-spec-tests migrated into ethereum/execution-specs; resolve current fixture sources instead of assuming old repository names remain active. Research implementation status at the pin; do not present a proposal, hybrid client or experimental component as a deployed feature. Separate throughput from latency and single-node performance from network finality. Do not transplant another chain's VM, consensus, object model, hash commitments or scheduling assumptions into XDC.

Coverage is bounded, not "all codebases in existence": complete the required families, investigate high-impact unresolved candidates, and run two discovery passes that produce no new relevant mechanism or independently actionable gap. Record searched terms, candidates and exclusions. Stop with an explicit coverage boundary and open research questions. Do not invent findings to satisfy an issue-count target.

PHASE 2: CRITIQUE AND RESOLVE ARCHITECTURE QUESTIONS
Inspect each topic below. Create an issue only when the evidence supports a defect, missing requirement or bounded decision spike; merge overlapping symptoms under one cause.

A. Consensus and compatibility
- All accepting nodes, including followers, need exact consensus checks. An approximation that accepts an invalid epoch list is not acceptable.
- Define canonical bytes, fork rules, chain IDs, balances, gasless/special transactions, TRC21 fees, reward/penalty windows, header/body/receipts validation and QC/TC rules from pinned evidence.
- Prove the actual domain of BigBalance. A carry limb or 33-byte example is not proof of an upper bound. Cover credits, debits, BALANCE, affordability, fees, nested reverts, selfdestruct, journaling, trie/RLP, snap, restart and unwind.
- Test proposed revm integration and Reth SDK extension points before committing to a standalone rewrite or a permanent fork. Compare supported hooks, small upstream contributions, a narrow adapter, controlled vendoring and alternative reuse. Measure maintenance burden and correlated dependency risk.

B. Oracle and test independence
- Use pinned Geth-XDC as a compatibility oracle, not an infallible specification of desired future behavior. Adjudicate contradictions against historical chain data and independently reviewed rules.
- Separate isolated prestate replay from sequential replay. Long replay must use xOne's own persisted previous state; resetting prestate from the oracle for every block can hide state accumulation defects.
- Include historical positive vectors, generated invalid blocks/messages, metamorphic/property tests, differential tests, deterministic network/storage simulation and mutation checks. The "only real archive vectors" rule is insufficient for rejection paths.
- Recompute header and transaction/receipt commitments independently. Comparing received headers with themselves proves nothing. Producer tests validate allowed behavior; valid producers need not choose identical transaction order, timestamps or extraData.
- Keep mutation injection outside shipping artifacts. Test that release builds cannot enable mutants or validation bypasses.

C. Durability and signing
- A database transaction does not automatically make separate static files atomic. Specify commit order, durable publication marker, file synchronization, recovery/truncation and restart invariants across all stores.
- Keep anti-equivocation state monotonic across reorgs, datadir restores, failover and cloned hosts. Separate reversible chain state from non-reversible signing safety. Define signer identity, fencing and refusal behavior on uncertain durability.
- Run storage fault, unwind and vote-path crash gates before any producer qualification. kill -9 is not a power-loss/fsync-failure model. Inject torn/short writes, disk-full, I/O errors, delayed flush, stale restore and network partitions.
- Define retention from each rule's actual dependency window, finalized anchor and reorg policy. Two epochs and 10 GB are hypotheses requiring evidence. Hash-pinned downloads prove byte integrity, not canonicality or checkpoint trust.

D. Performance and operations
- Define separate SLOs for decode/admission, cryptographic checks, state access, execution, root computation, durable commit, propagation and finality. Nanoseconds are suitable for selected CPU microbenchmarks, not a blanket end-to-end requirement.
- Benchmark p50/p95/p99/p99.9 plus throughput, queue depth, saturation, error rate, RSS/cgroup/page cache, I/O amplification and CPU. Pin hardware, power/NUMA settings, OS/filesystem, dataset, versions, sync/prune mode, workload, warm/cold state, load generator and durability flags.
- Include adversarial/hot-account conflict workloads, EVM-heavy and storage-heavy blocks, RPC/sync interference and PQ payloads. Use measured bottlenecks before choosing parallel execution, SIMD, NUMA pinning or kernel bypass. Preserve deterministic canonical results and a serial fallback.
- A 1,800-global-block run with about 97 validators gives only about 18-19 opportunities per validator. Specify opportunity counts, epoch diversity, fault scenarios and uncertainty; "100% observed" is not a reliability guarantee.
- Correct memory accounting and bootstrap network/disk assumptions. Replace fixed footprint claims with workload-scoped targets and growth measurements.
- Verify lint rules actually exist and work at the pinned toolchain; use compile-fail fixtures or custom checks where necessary. Audit transitive unsafe/FFI boundaries. Add RPC quotas, cancellation/backpressure, peer DoS budgets, logs/disk limits, release provenance, SBOM and data-format migration/rollback.

E. Quantum migration
- Track A: crypto inventory, algorithm/version/domain-separated interfaces, test vectors, provider isolation, benchmark harnesses and capability design that preserve current XDC consensus.
- Track B: research proposals for PQ transaction/accounts, validator votes and certificates, peer authentication/key establishment, release/snapshot signatures, bridges, wallets, hardware signers and legacy recovery. Protocol activation requires a coordinated network decision.
- ML-KEM establishes shared secrets; it is not a transaction signature. Assess ML-DSA and SLH-DSA for distinct roles using current standards and implementation evidence. Define downgrade protection and hybrid acceptance precisely; classical OR PQ acceptance does not provide PQ authentication.
- Budget public-key/signature sizes, certificate fan-out, verification work, gas and network DoS. Do not assume compact BLS-like aggregation exists for the chosen PQ scheme.
- Address exposed/reused keys, contract owners, ecrecover dependencies, legacy balances, migration authorization before classical keys can be forged, checkpoint/history authentication and rollback/downgrade. Preserve existing hash commitments unless a separately reviewed protocol proposal changes them.
- Pin algorithm revisions, parameters, applicable errata and exact implementation versions. Record independent audit scope, constant-time/side-channel evidence, entropy/zeroization, platform support, unsafe/assembly/FFI and supply-chain review. A standard algorithm name is not an audited or FIPS-validated module. Treat unaudited libraries and prototyping libraries as lab inputs until reviewed.
- Say "quantum-resistant under stated assumptions" or "PQC-ready"; never promise unconditional quantum proof.

PHASE 3: CREATE DEDICATED ISSUES
Fetch open and relevant closed issues/PRs first. Map every finding to an existing issue, a new issue, a rejected hypothesis or an explicit deferred research question. Use one issue per independently verifiable outcome; use parent milestone issues only for coordination. Do not combine all ADR decisions into a single acceptance checkbox.

Each issue must contain exactly these 13 headings:
1. What: concrete change, goal, affected paths/symbols, track, priority, scope and stable finding ID.
2. Why: failure scenario, user/operator/developer impact, pinned evidence, source links and assumptions. Explain relevant reference mechanisms and license/reuse limits.
3. 80% of the way to: testable acceptance criteria, remaining uncertainty, benchmark/invariant gates, dependencies, non-goals and rollback. This is an implementation-ready specification, not permission to leave 20% unverified.
4. ~ASD-STE100: short plain-English facts, actions, conditions and pass/fail statements. Keep exact identifiers. Do not claim certified compliance.
5. Diagrams: compact Mermaid for the architecture or state transitions affected; maximum five nodes horizontally. Use "Not applicable" with a reason when justified.
6. Images: actual reproducible chart, profile, packet layout or rendered diagram evidence when useful. Otherwise state the needed artifact or why none is needed. Never fabricate screenshots, measured charts or decorative AI architecture pictures.
7. Prompt: self-contained executor instructions with pins, allowed paths, inputs, output, required checks and forbidden shortcuts.
8. Workflow: ordered implementation and verification steps, owner/reviewer roles and evidence outputs.
9. Loop: failure/repair/retest logic, escalation, progress checkpoints and explicit success/failure stopping conditions.
10. Graph: dependency edges and blocking issues; use local stable IDs until actual issue URLs exist.
11. Layout: affected files/modules, public interfaces, dependency direction and evidence paths; do not force an unrelated UI layout.
12. Flow: relevant runtime/protocol/data sequence and failure/recovery behavior.
13. Summary: one short paragraph with next action and decisive gate.

Write bodies locally before posting. One coordinator owns external issue writes and the issue ledger. Add a stable HTML marker such as <!-- xone-audit:<finding-id>:<fingerprint> -->. Before each create, search the ledger and remote issues for that marker and semantic duplicates. Preserve unrelated issue text when updating. Post in dependency order, retain returned issue URLs, then reconcile every dependency and parent checklist. Never fabricate issue numbers or claim creation without a confirmed response.

PHASE 4: HAND OFF THE GOAL, WORKFLOW AND LOOP
After every current finding is reconciled and every intended issue write is confirmed or explicitly blocked, produce a concise execution plan containing:
- Goal: XDC-compatible, independently verified client with measurable latency/resource targets; parallel PQ migration research under explicit protocol constraints.
- A coverage ledger with verified/reported/unverified claims, issue URLs, deduplicated findings and deferred questions.
- Revised milestone DAG. Place persistence, signer crash safety and recovery gates before producer qualification. Distinguish parallel investigation from milestones whose acceptance depends on integration.
- The critical path and next three to five ready issues, each with dependencies, deliverable and acceptance gate. Start with evidence/pins, revm/BigBalance feasibility, storage publication/recovery and benchmark specification when the audit supports that order.
- A language ADR: Rust and Go as primary candidates, plus C++, Java/C#, Zig/Nim and any justified alternative. Discuss correctness, ecosystem/compatibility, team cost, tail latency, FFI, build times and upstream maintenance. Do not invent a speed ranking.
- An execution loop: select ready issue -> establish failing evidence -> implement one bounded change -> targeted checks -> relevant differential/fault/performance gate -> independent review -> PR -> record result. Full replay at justified integration boundaries; do not repeat millions of blocks after an unrelated prose edit.
- An explicit stop point: this run ends with issue creation and the reviewable execution recommendation. Implementation, live rollout and protocol activation are separate scopes.

AGENTS, TOKEN BUDGETS AND RESUMPTION
Use at most six concurrent agents in total, including coordinator and independent verifier, within actual CPU/RAM/provider quotas. Use one coordinator and up to four specialist workers: compatibility/state, storage/consensus safety, performance/languages, PQ/security. Use an independent verifier when a slot is free. Read-only research may overlap; assign disjoint file ownership for local writes. Only the coordinator writes GitHub issues. Do not run concurrent full-chain replays or broad repository crawls without a resource budget.

Send workers bounded questions, exact pins/paths and short acceptance criteria. Require compact findings plus source artifacts, not full repository dumps. Reuse cached source facts keyed by SHA. Search with rg and read relevant code slices before expanding. Route routine extraction to available lower-cost models only when the environment supports them; escalate ambiguous consensus/security decisions. Do not invent model switching capabilities.

Checkpoint after each phase, completed work item, external write and material decision. At context pressure, update session.json and handoff.md with exact next actions. New sessions read the checkpoint, verify pins/remote issue state and resume remaining work. Do not rescan completed immutable evidence or recreate confirmed issues.

On 429/secondary limits, honor Retry-After or reset metadata, reduce concurrency and apply bounded exponential backoff with jitter. Store retry time, attempt count and pending action. Never bypass limits through identity/key rotation. A timed-out write has unknown outcome: query by marker before retrying. If continuation cannot run inside the current session, return a precise resumable checkpoint and command; do not claim an unattended scheduler exists. Escalate after three failed verification attempts on one unchanged approach or a predeclared resource budget, with evidence and alternatives.

BEGIN NOW
Read the source plan and repository instructions, establish pins and existing-issue inventory, run the bounded parallel audit, create the evidence-backed issues, reconcile the issue ledger, and finish with the execution goal, next batch, workflow, loop and unresolved decisions. Complete all authorized investigation without repeated confirmation requests.
```
