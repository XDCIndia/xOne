# Assurance checks (XA-10, XB-02 CI rows)

Plan #1 R3 rules as checks that fail. GitHub Actions is on a billing hold, so every command below
is run locally; `.github/workflows/ci.yml` runs the same commands. Toolchain: stable 1.94.0
(`rust-toolchain.toml`). Date of the runs: 2026-11-11.

## R3 rule 1 and 2: mechanism per rule

Lints live in `[workspace.lints]` (root `Cargo.toml`). A crate opts in with `[lints] workspace = true`.
Opted in: `xone-chainspec`, `xone-consensus`, `xone-evm`, `xone-state`, `xone-primitives`, `xone-signer`.
Opted in: `xone-chainspec`, `xone-consensus`, `xone-evm`, `xone-state`, `xone-primitives`, `xone-net`.
`disallowed-*` lists are in `clippy.toml` of each opted-in crate; `ci/rule2.clippy.toml` is the master copy
and `ci/check-fixtures.sh` fails if a copy differs. (A root `clippy.toml` would also hit `build.rs`,
`sim` and the spikes, which read the environment on purpose.)

| Rule | Mechanism at 1.94.0 | Fixture (`ci/fixtures/`) |
|---|---|---|
| `unsafe_code` (rule 1) | rustc lint `unsafe_code = "forbid"` | `unsafe-code` |
| `unwrap_used` | clippy restriction lint | `unwrap-used` |
| `expect_used` | clippy restriction lint | `expect-used` |
| `unwrap_or_default` | **The lint of that name does not ban the call**: it is a style lint that recommends it. The ban is `disallowed-methods` for `Option::unwrap_or_default` and `Result::unwrap_or_default`. The lint is also set to deny. | `unwrap-or-default` |
| `.ok()` on a dropped `Result` | `clippy::unused_result_ok` (restriction, exists at 1.94.0) | `unused-result-ok` |
| `as` numeric casts | `clippy::as_conversions` (all `as`, not only numeric) | `as-conversions` |
| `std::env::var` | `disallowed-methods`: `var`, `var_os`, `vars`, `vars_os` | `env-var`, `env-var-os` |
| `OnceLock`, `LazyLock` | `disallowed-types` (also `OnceCell`, `LazyCell`) | `once-lock`, `lazy-lock` |
| `static mut` | Use needs `unsafe`, which `unsafe_code = forbid` rejects (fires). A bare declaration compiles, so `ci/check-globals.sh` greps it. No Clippy lint bans the declaration. | `static-mut` (clippy), `globals-grep/static-mut-decl.rs` (grep) |
| `lazy_static`, `once_cell` | grep in `ci/check-globals.sh` (a `disallowed-types` entry for a crate that is not a dependency is itself a Clippy error) | `globals-grep/lazy-static.rs`, `globals-grep/once-cell.rs` |

Each fixture is one `lib.rs` whose first line is `// expect: <text>`. `ci/check-fixtures.sh` builds it in a
throwaway crate with the lint tables copied from the root manifest, requires `cargo clippy -D warnings`
to fail and the output to contain `<text>`. A clean control file must pass, so the failures are the rules.

Command: `ci/check-fixtures.sh` (also runs the layering fixtures, `check-globals.sh`, and
`cargo clippy --locked --all-targets -p <crate> -- -D warnings` for each opted-in crate).

Rule 3 (typed "missing"), rule 4 (sync consensus, two traits) and the `sanitizer`/`loom` half of rule 5 are
design rules without a mechanical check here.

## Allow count in the rule crates

Where code broke a rule it was fixed if that kept behaviour bit for bit (`as u128` to `u128::from`,
`bool as u8` to `u8::from`, `len() as u64` to `u64::try_from`). Otherwise there is one `#[allow]` with its reason on the line.
Consensus and EVM behaviour is unchanged.

| Crate | Per-site allows in `src/` | Test-only allows | Sites |
|---|---:|---:|---|
| xone-chainspec | 3 | 2 | `as_conversions` (enum index), `expect_used` in `spec()` (embedded table, loaded by a test), `unwrap_or_default` (absent genesis `code`); tests: `XONE_BLESS` `env::var_os` x2 |
| xone-consensus | 4 | 0 | `as_conversions` x2 (enum discriminant), `unwrap_or_default` x2 (absent lock QC encodes as zero ref; `take(8)` is always 8 bytes) |
| xone-evm | 2 | 3 files | `unwrap_or_default` (no blob gas price is zero), `as_conversions` (`i64` refund counter to `u64`); 3 integration-test files |
| xone-state | 1 | 4 files + 1 module | `unwrap_or_default` (unset slot reads zero); integration tests and `tests/common` |
| xone-primitives | 1 | 2 files | `as_conversions` (`#[repr(u16)]` enum id); `unwrap`/`expect` in 2 integration-test files |
| xone-signer | 0 | 4 files | `unwrap`/`expect` in 3 integration-test files and `tests/common` |
| Total | 11 | 2 sites + 14 files | |

`allow-unwrap-in-tests` and `allow-expect-in-tests` are set in `clippy.toml` for `#[test]` and `cfg(test)` code.
Integration-test files have a file-level allow, because Clippy does not treat their helper functions as tests.

Open item for the integrator: `Reader::u64` in `xone-consensus` decodes with `try_into().unwrap_or_default()`.
The default is unreachable today, but a short read would decode as zero. Change it to a typed error when the
safety-record format is next touched (not done here: no behaviour change).

## Layering (plan #1 Layout)

Command: `ci/check-layering.sh` (rules in `ci/layering.py`); proof: `ci/check-layering.sh --fixtures`.

- Rank: primitives 0, chainspec 1, state = evm 2, consensus 3, db 4, net = snap = sync 5, builder = rpc 6,
  node 7, `bin/xone` 8. A crate may depend only on a strictly lower rank (normal, build and dev edges).
- `sim/`, `spikes/`, `labs/`, `conformance/` may depend on anything; nothing outside them may depend on them.
  That includes `bin/xone`.
- Allow-list: `crates/<name>` must be one of the names above, `bin/xone` is `xone`. Anything else fails.
- Fixtures (`ci/fixtures/layering/cases.json`): wrong direction, same rank, depending on `sim`, `bin/xone` on a lab,
  an unknown crate, and a good case (db depends on consensus).

Decision on `xone-consensus -> xone-db`: plan #1 Layout puts `xone-db` after `xone-consensus`, so the allowed
direction is **db depends on consensus**. Today `xone-consensus` uses `xone_db::{Durability, Cause}`
(`v2/safety.rs`), so the rule has one named exception in `EXCEPTIONS` in `ci/layering.py`, printed on every run
and only for that exact normal edge. The build is not broken. The plan's own sequence diagram has consensus
writing the safety record to the db, so the integrator should choose: move `Durability`, `Cause` and the
`SafetyStore` trait into `xone-consensus` (db implements it) and delete the exception line. Not done here:
it moves types between crates, which is more than an assurance change.

## Supply chain

| Check | Command | Result 2026-11-11 |
|---|---|---|
| cargo-deny 0.20.2 | `cargo deny --locked check` (`deny.toml`) | advisories ok, bans ok, licenses ok, sources ok; duplicate versions warn only |
| cargo-audit 0.23.0 | `cargo audit --deny warnings` (`.cargo/audit.toml`) | pass with three ignores below; without them: 1 vulnerability, 2 unmaintained |

Ignored advisories, each with a reason in the config:
- RUSTSEC-2026-0387: `k256 0.13 -> crypto-bigint 0.5.5` (timing variability). Fix is crypto-bigint 0.7.6,
  which k256 0.13 cannot take. In the `xone` release tree. See #70. Needs an owner decision.
- RUSTSEC-2024-0436: `paste` unmaintained, a proc-macro behind `alloy-primitives`.
- RUSTSEC-2024-0388: `derivative` unmaintained; in `Cargo.lock` through `ark-ff` but compiled for no target.
  (cargo-audit reads the lockfile; cargo-deny does not report it.)

Licenses: our crates `GPL-3.0-or-later`; dependencies allowed: MIT, Apache-2.0 (and with LLVM exception),
BSD-2/3-Clause, Unicode-3.0, Zlib, CC0-1.0, Unlicense. Other permissive licenses (ISC, MPL-2.0, 0BSD) are not
in the allow list because no package in the tree uses them; add one on first need. Sources: crates.io and the
reth git URL used by `spikes/net` (pinned by revision).

## Unsafe and FFI audit

Command: `ci/unsafe-audit.sh > docs/assurance/unsafe-ffi-audit-2026-11-11.md`. `cargo geiger` is not used
(see the script). Table: [unsafe-ffi-audit-2026-11-11.md](unsafe-ffi-audit-2026-11-11.md). Owner review: pending for every row.
The FFI column is a text heuristic; the two hits (`alloy-primitives`, `crypto-bigint`) are a wasm-only
`extern "C"` import block and a fuzz entry point, not native libraries linked on Linux.

## Release tree

Command: `ci/check-release-tree.sh` (CI sets `RELEASE_TREE_REQUIRE_CLEAN=1`). It builds `-p xone` in release and checks
(a) `cargo tree -p xone -e features` has no `fault-injection`, `mutants`, `account-ext`, lab or test-only crate,
(b) `nm` and `strings` of the binary show no `xone_sim`, `pq_lab`, `wide_balance_spike`, `net_spike`, `xone_sweep`,
(c) `xone --version` has no `unknown`, and no `-dirty` on a clean tree. On a dirty tree it expects `-dirty` and says
that the clean-tree assertion was not exercised. Build `-p xone`, not the whole workspace: a workspace build
unifies `fault-injection` into `xone-db` for `sim`.

## Miri (R3 rule 5, primitives)

Command: `cargo +nightly miri test -p xone-primitives --lib` (needs `rustup component add miri rust-src --toolchain nightly`).
CI pins `nightly-2026-03-24`, the nightly installed locally (1.96.0-nightly). Local result: 8 passed, 0 failed,
850 s (the k256 sign and recover test is most of it). The integration tests (`tests/vectors.rs`, `tests/wide_balance.rs`)
were NOT run under miri: they open vector files, which miri isolation rejects (`open not available when isolation is enabled`),
and a run with `-Zmiri-disable-isolation` was not attempted because of the time. `xone-consensus` miri is not in CI yet.

## Not covered

SBOM and build provenance (#15 last box), the reproducible-build hash on a second builder (#22), CI on GitHub
(billing hold), `cargo auditable` (see below), `fuzz/`, the sanitizer or `loom` runs of rule 5, rules 3 and 4 as checks.
`cargo auditable build` embeds the dependency list in the binary: it needs `cargo install cargo-auditable --locked`,
the release build to go through it, and `cargo audit bin target/release/xone` in CI. An SBOM needs `cargo cyclonedx`
or `cargo sbom`, and provenance needs a signed attestation from the CI runner, so both wait for CI.

## Release build (XB-26, issue #46)

Reproducible build, SBOM and provenance rows of #73: `ci/release.sh` builds with `cargo auditable --locked`,
`SOURCE_DATE_EPOCH` = commit time, path remap and no build id; writes a CycloneDX 1.5 SBOM, an in-toto/SLSA v1
provenance file, `SHA256SUMS` and a gpg detached signature. `ci/test-release.sh` runs it in two clones at
different paths (equal binary and tarball hashes on 2026-11-12), `ci/test-packaging.sh` runs shellcheck,
`systemd-analyze verify` and the launcher presets, `ci/rehearse-release.sh` runs upgrade and rollback.
Records: `docs/ops/release.md`, `docs/ops/release-rehearsal.md`. Gaps: two builders on separate machines,
a different `CARGO_HOME`, signing key custody (owner), SBOM and provenance not signed beyond `SHA256SUMS`,
no aarch64, no CI run on a tag.
