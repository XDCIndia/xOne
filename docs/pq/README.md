# Post-quantum readiness, track A

Work item [#17](https://github.com/XDCIndia/xOne/issues/17) (XA-12): cryptographic inventory, threat model and versioned crypto interfaces. This directory is research and compatible interface work. It changes no consensus rule and no wire byte.

The goal is PQ migration readiness under stated assumptions. It is not "quantum proof": no language and no single library can promise resistance to all future attacks.

## No-activation rule

1. Nothing in this directory or in `crates/xone-primitives/src/crypto.rs` activates a new algorithm. `AlgorithmId::Secp256k1Keccak = 1` is the only valid value; every other id, including the experimental range `0xF000..=0xFFFF`, is an error.
2. A single client cannot activate incompatible rules. A PQ change is a separate, versioned, coordinated proposal with a written specification, review, an independent audit, vectors that pass in at least two independent clients, a multi-client devnet run, wallet and signer readiness, and a governance decision (Gate Q in the `xone-rs-pq-readiness` skill).
3. The wire and consensus freeze of the compatible rebuild (plan B10) stays.
4. Lab implementations of PQ schemes live in a non-shipping crate and are reachable only from test builds (#18). None exists yet.
5. Claims about security, size or speed cite a pinned source or a measurement with its inputs. Arithmetic is labelled as arithmetic.

## Documents

| File | What it is | Issue |
|---|---|---|
| [`inventory.md`](inventory.md) | One row per cryptographic use and authority: 56 on-chain rows (T01-T56) and 14 planned rows (P01-P14), each with algorithm, key owner, public-key exposure, message domain, consensus-critical flag, and geth file and line at the pin | #17 |
| [`threat-model.md`](threat-model.md) | Harvest-now-forge-later per signature class, exposed and reused keys, legacy balances, downgrade paths, standards state with errata, and the decisions left to Track B | #17 |
| [`../../crates/xone-primitives/src/crypto.rs`](../../crates/xone-primitives/src/crypto.rs) | `AlgorithmId`, `Domain`, `Signature`, `Signer`, `Verifier`, and `Secp256k1Verifier` wrapping the existing `ecrecover` | #17 |

## How the work items connect

- **#17 (this).** Inventory, threat model, interfaces. Acceptance: inventory complete, threat model written, interface with algorithm id and domain tag, current secp256k1 the only active implementation, current vectors unchanged. The vector test `crypto_interface_matches_seal_path_and_geth` in `crates/xone-primitives/tests/vectors.rs` runs every header vector (Apothem and net2040, 2,500 headers) through `Secp256k1Verifier` and requires the same signer as `v1_signer` and `v2_signer` and as the geth-XDC signer recorded in the vector.
- **#18, track A.** Pinned standards, implementation-assurance register and cost lab. It takes the standards table and the open questions Q1 and Q2 in `threat-model.md` and the QC-size arithmetic (section 8), and turns arithmetic into measurements with host, CPU and commit.
- **#19, track B research.** Account, wallet and validator signature migration. It takes `threat-model.md` sections 3 to 5, 7, 8 and the item list in section 9, and the rows T01-T22, P01-P06.
- **#20, track B research.** Transport identity, checkpoints and history, bridges, release signing. It takes sections 4 (item 3), 6, 8, 9 and the rows T26-T30, T42-T47, P07-P10.
- **Blocked by #6** (rule register). **Related:** #14 (authenticated bootstrap anchor), #40 (key custody), #46 (release; the release key must be recorded in the inventory when it exists), #12 (signer safety).

## Interface summary

- `Signer::sign(domain, digest) -> Signature` and `Verifier::recover(domain, digest, &Signature) -> Address`. A `Signature` carries its `AlgorithmId`; a verifier never infers the algorithm from byte length.
- `Domain` has one variant per message domain in the inventory. For secp256k1 the domain is ignored, so every signed and verified byte is today's byte. `Domain::tag()` gives a stable ASCII context for algorithms that bind one (FIPS 204 allows up to 255 bytes).
- `recover` is shaped for recoverable ECDSA. A PQ scheme verifies against a registered public key; Track B decides that shape. The trait is internal and can change.
- Not covered by the wrapper: the low-S rule geth applies to transactions and V2 certificates and the 7702 authorization checks. See the inventory notes for the geth lines.
- Files changed outside the issue's `crypto*` path glob, by instruction of the task: `crates/xone-primitives/src/lib.rs` (module and re-exports) and `crates/xone-primitives/tests/vectors.rs` (reuse of the vector harness). `seal.rs` is unchanged and its public API is unchanged.

## Verify

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

## Open items before #17 closes

- A fourth discovery pass over the pin (pass 3 added seven rows; #17 asks for two passes with no new rows). The inventory section D lists the passes run.
- Unknowns recorded in the inventory: T19, T20, T21, T24, T42, T44, P03, P06, P07, P10.
- Questions Q1 to Q5 in `threat-model.md` section 9.
- Review by the security owner (the issue's named reviewer).
- Re-pin: the issue asks to re-pin before work and record the new SHA in the PR. This work used the pin in `conformance/PIN` (`6fc8e8c2088b7f18264f5663036669d04fa7becf`) and did not re-pin.

## Documents

| Document | Issue | Content |
|---|---|---|
| [inventory.md](inventory.md) | #17 | every cryptographic use and authority, on chain today and planned |
| [threat-model.md](threat-model.md) | #17 | harvest-now-forge-later, exposed keys, downgrade paths |
| [standards.md](standards.md) | #18 | FIPS 203/204/205, SP 800-208/230, IR 8547, errata status |
| [implementations.md](implementations.md) | #18 | implementation-assurance register (lab inputs until reviewed) |
| [lab-results-2026-11-11.md](lab-results-2026-11-11.md) | #18 | single-host cost measurements under docs/perf.md, not targets |
| [bandwidth-model.md](bandwidth-model.md) | #18 | tx, vote and QC fan-out arithmetic, no aggregation assumed |

The lab code lives in `labs/pq` and is excluded from the shipping binary; `labs/pq/tests/no_ship.rs` proves it.
