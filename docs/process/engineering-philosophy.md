# xOneRS against Lambda Class's engineering philosophy

Source: [Lambda's engineering philosophy](https://blog.lambdaclass.com/lambdas-engineering-philosophy/) (LambdaClass, 27 Sep 2023). Assessed tree: `origin/main` at `bd1e0c3` (merge of PR #151), 2026-11-14. Quotes are verbatim from the post. Every finding cites a file in this repository or a URL.

Ratings: **follows** (the practice is in place), **partly** (in place with a named gap), **deviates** (the practice is missing or broken). Each deviation has one issue with the label `philosophy`.

## Summary

| # | Principle (post quote) | Rating | Evidence (short) | Issue |
|---|---|---|---|---|
| 1 | Curiosity and production: "feel the joy of putting things into production" | follows | Followers run on net2040 and Apothem (`docs/ops/net2040-follow.md`, `docs/ops/apothem-follow.md`); local mint run (`docs/ops/first-mint-local.md`) | - |
| 2 | Observe and measure: "You cannot solve a problem you cannot see. Seeing is measuring." | partly | Contract and procedure exist (`docs/perf.md`, `bench/README.md`) but both say no baseline was run | covered by #13 |
| 3 | "Iterate frequently in your perceive-act loop to shorten feedback." | follows | 192 PRs; 111 CI runs from 2026-11-11 to 2026-11-14 (GitHub Actions API) | - |
| 4 | "Once a problem has been identified, speak openly about it." | follows | Public repo; 188 issues; `.audit/findings.jsonl`; `docs/reviews/` | - |
| 5 | "KISS, Keep It Simple, Silly." | partly | 14 product crates, 55,803 src lines; `#![forbid(unsafe_code)]` in all 14 `lib.rs`; dead and duplicate code in `docs/process/ponytail-audit.md` | #206-#211 |
| 6 | Dogfooding: "Repositories should build easily and cleanly on all of the target environments." | **deviates** | 0 of 111 CI runs succeeded; `fork-table-drift` fails without `GETH_XDC_DEPLOY_KEY`, `miri` hits the 6 h job limit (`.github/workflows/ci.yml:55-89`) | #203 |
| 7 | "Do not generalize until you absolutely need it." | partly | 46 traits in `crates/*/src`; most have a real and a test implementation; 5 have none (`crates/xone-db/src/lib.rs:68-100`) | #207 |
| 8 | Experiment: "Go fast, try things out, fail quickly, figure it out" | follows | `spikes/wide-balance/REPORT.md`, `spikes/net/REPORT.md`, `spikes/sdk-leaf/REPORT.md` fed `docs/adr/ADR-002.md` | - |
| 9 | "Process is the sign of a mature project" ... "it should not be introduced before it's necessary" | partly | Gates, layering and unsafe checks are justified for consensus code (`ci/check-layering.sh`, `docs/assurance/README.md`). All four ADRs are still "Proposed" while the code is built on them (`docs/adr/*.md` line 3); finished spikes still build in the workspace (`Cargo.toml` members) | covered by #8, #11, #12, #16; spikes: #206 |
| 10 | "a part of it has to revert to the experimentation phase." | follows | `labs/pq` (#18), storage decision #135, `docs/ops/disk-state.md` | - |
| 11 | "The first vertical slice should be bare bones" with "db, api, networking, ci, testing and linting" | follows | Gate DAG #5 and #57; slices A-E; every layer exists in the tree (`crates/xone-db`, `xone-rpc`, `xone-net`, `ci/`) | - |
| 12 | Plan: estimate, "multiply that number by 1.5 or 2", then a Gantt chart | **deviates** | `docs/plans/2026-11-11-execution-plan.md`, #5 and #57 order the work; no estimate or date appears (`grep -iE 'estimat|gantt|weeks?' docs/plans` is empty) | #204 |
| 13 | "Readability counts." | partly | Short commit and doc style; but 7 src files are over 1,000 lines, largest `crates/xone-sync/src/mdbx/mod.rs` (2,287) | - |
| 14 | "Take pride in having an up-to-date readme" | **deviates** | `README.md:7` reviews PR #93 (main is at #191); `README.md:17` names `FileStore`, which no longer exists in `crates/`; no link to the 11 guides in `docs/ops/` | #205 |
| 15 | "Thorough testing and code review thus become key." | follows | 704 `#[test]`, 9 `proptest!` blocks, 3 fuzz targets (`fuzz/fuzz_targets/`), miri on `xone-primitives`, differential replay against geth-XDC (`conformance/harness/RUNS.md`) | - |
| 16 | Few dependencies (owner's checklist; the post does not state it) | partly | 379 crates in the `xone` binary graph (`cargo tree -p xone -e normal`); 496 `Cargo.lock` entries; 18 crates in two versions (`cargo tree -d`), two libsecp256k1 copies; controlled by `deny.toml` and the ADR-002 deny list | - |

Counts: **7 follows, 6 partly, 3 deviates.**



## Evidence

### Size and shape

| Crate | src lines | test lines |
|---|---:|---:|
| xone-node | 12,852 | 1,912 |
| xone-sync | 10,674 | 6,326 |
| xone-consensus | 6,719 | 6,344 |
| xone-builder | 3,705 | 3,849 |
| xone-net | 3,643 | 1,436 |
| xone-rpc | 3,564 | 1,001 |
| xone-snap | 2,920 | 1,372 |
| xone-db | 2,535 | 0 (tests in `tests/fault`) |
| xone-state | 2,085 | 1,580 |
| xone-evm | 1,892 | 1,085 |
| xone-executor | 1,665 | 152 |
| xone-primitives | 1,470 | 586 |
| xone-chainspec | 1,437 | 143 |
| xone-signer | 642 | 640 |

Method: `find crates/<c>/src -name '*.rs' | xargs cat | wc -l`, same for `tests/`. The workspace has 26 members (`Cargo.toml` line 3): 14 product crates, `bin/xone`, `sim`, 7 conformance and test harnesses, 2 spikes and `labs/pq`.

Release binary: 19,430,648 bytes (`target/release/xone`, built 2026-11-13 from the main checkout). Build time was not measured for this document.

### Dependencies

- Direct external dependencies per crate are small and named in `[workspace.dependencies]` (`Cargo.toml`). The largest sets are in `crates/xone-node/Cargo.toml` (tokio, rustls, webpki-roots, tungstenite, tracing-subscriber, flate2, secp256k1) and `crates/xone-signer/Cargo.toml` (keystore v3 crypto).
- reth is used only for `reth-ecies` and `reth-network-peers` by git revision (`crates/xone-net/Cargo.toml:19-20`), as `docs/adr/ADR-002.md` decision item 3 says.
- Duplicates from `cargo tree -d`: `secp256k1` 0.30 (`reth-ecies`, `crates/xone-net`) and 0.31 (`alloy-primitives`); `sha2`/`sha3`/`digest` 0.10 and 0.11 (`k256` 0.13 and `revm-precompile` 43); `getrandom` 0.2, 0.3 and 0.4; `rand` 0.8 and 0.9; `syn` 2 and 3. All come from upstream pins, not from our manifests.
- Unused direct dependencies: `serde` in `crates/xone-rpc/Cargo.toml:21`; `alloy-rlp` in `crates/xone-chainspec/Cargo.toml:14` is used only by tests (ponytail audit).

### Unsafe and FFI

`unsafe_code = "forbid"` at workspace level (`Cargo.toml:53`) and `#![forbid(unsafe_code)]` in each of the 14 `crates/*/src/lib.rs`. The C code comes in through `libmdbx = "=0.6.6"` (pinned, `Cargo.toml`), `secp256k1` (libsecp256k1) and `ring` (rustls). The audit is `docs/assurance/unsafe-ffi-audit-2026-11-11.md`; the check is `ci/unsafe-audit.sh`.

### Abstraction

46 public traits in `crates/*/src`. Count of `impl <Trait> for` in `crates/*/src` (all impls including tests in brackets):

- 0 (0): `HeaderProvider`, `BodyProvider`, `ReceiptProvider`, `StateProvider`, `BlockWriter` in `crates/xone-db/src/lib.rs:68-100`.
- 1: `ChainRead` (`crates/xone-sync/src/chain/mod.rs:35`, used as `&dyn` by tests and `bin/xone`), `Verifier` (`crates/xone-primitives/src/crypto.rs:151`, the #17 crypto interface), `RemoteSigner` (`crates/xone-signer/src/remote.rs:36`, the #40 interface), `Io` (a `Read + Write + Send` alias, `crates/xone-node/src/observe/wire.rs:12`).
- 2 or more: the rest, usually one production and one in-memory implementation (for example `SafetyStore`, `crates/xone-db/src/safety.rs`).

### Docs and process

- ADRs: `docs/adr/ADR-002.md`, `ADR-005-storage-and-publication.md`, `ADR-007-operations-limits.md`, `ADR-010-signer-safety.md`. Each says "Proposed" on line 3.
- Spec pages: 84 files in `spec/`. Ops guides: 11 files in `docs/ops/`. Assurance: `docs/assurance/README.md`.
- Commit, PR and release rules: `.claude/skills/xone-rs-release/SKILL.md` (local gates: fmt, clippy, nextest, deny, audit, layering, miri).

### Tests and gates

- 704 test functions, 9 `proptest!` blocks, 3 fuzz targets.
- CI (`.github/workflows/ci.yml`): `check` (fmt, clippy `-D warnings`, `cargo test --locked`, Python tests), `assurance` (fixtures, layering, cargo-deny, cargo-audit, release tree), `miri` (`xone-primitives --lib`), `fork-table-drift` (needs the private oracle).
- Result: GitHub Actions reports 111 runs and 0 successes (`gh run list --repo XDCIndia/xOneRS --status success` is empty). The run for PR branch `feat/disk-trie` (run 43513275926) shows `check` and `assurance` green, `fork-table-drift` red in 8 s ("GETH_XDC_DEPLOY_KEY is unavailable"), `miri` failed at the 6 h job limit (6 h 0 m 16 s).

## Issues

| Rating | Issue | Title |
|---|---|---|
| deviates (6) | #203 | ci: no CI run has ever passed; make the required jobs green and the oracle job explicit |
| deviates (12) | #204 | plan: add rough estimates and a dated chart to the gate DAG |
| deviates (14) | #205 | docs: README describes PR #93, not main; bring it up to date |

Partly-rated principles that existing issues already cover are not filed again: measurement (#13), ADR acceptance (#8, #11, #12, #16). Lean findings are in `docs/process/ponytail-audit.md` with their own issues.
