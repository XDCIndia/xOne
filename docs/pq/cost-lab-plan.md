# PQ cost lab plan: the contract-compliant run

Work item: [#18](https://github.com/XDCIndia/xOneRS/issues/18) (XA-13), post-quantum track A. Research only. No activation, no security claim implied. Lab code stays in `labs/pq` (non-shipping, `labs/pq/tests/no_ship.rs`).

This is a plan. It runs nothing. The existing measurement is [`lab-results-2026-11-11.md`](lab-results-2026-11-11.md), a single shared-host run that its own text says is "not a target and not a requirement". This plan lists what is missing from it, what each missing piece costs to measure, and the decisions only the owner can make before the run.

## 1. What exists and what is open

| Item of #18 | State | Where |
| --- | --- | --- |
| Standards table with revision, parameter sets, errata | Done, re-checked on 2026-11-14 (section 5) | `standards.md` |
| Implementation register (version, licence, audit, constant time, entropy, platforms, unsafe) | Done for R1-R12; no library reviewed by a person, no NIST vectors run by this work | `implementations.md` section 8 |
| Lab results ML-DSA-44/65/87, SLH-DSA, valid and invalid verify, sizes | Measured once on a busy shared host (load average 14 to 72, 41 of 51 rows over tolerance) | `lab-results-2026-11-11.md` sections 2, 4, 5 |
| Bandwidth model for tx, vote and QC fan-out | Arithmetic from the lab sizes; no network measured | `bandwidth-model.md` section 4 |
| Lab code in a non-shipping crate | Done | `labs/pq`, `crates` default members exclude it (PR 213) |
| Run under the XA-08 contract (`docs/perf.md`) on a host that meets it | **Open** | this plan |

## 2. Workloads still to measure

Each row names the gap in the 2026-11-11 results (section and note number), the measurement, the output and the pass condition. IDs are new (`Q` is for the PQ lab).

| ID | Gap (source) | Measurement | Output | Pass |
| --- | --- | --- | --- | --- |
| Q1 | Noise: 41 of 51 rows over tolerance (`lab-results` section 4, note 8) | Re-run the existing `pq-lab` loop three times on a quiet host with the `docs/perf.md` section 2 record | Same table as section 4 of the results, new date | Every row's p50 spread is within `tolerance_pct` (default 10 percent), or the row is marked void |
| Q2 | ECDSA baseline built without precomputed tables (note 2) | Add a second ECDSA row: `k256` with its default features | ECDSA verify and recover with tables, next to the no-table row | Both rows reported; the ratio to ML-DSA-65 is stated for both |
| Q3 | Malformed encodings not tested (note 1) | Verify a signature whose encoding the decoder rejects at once (ML-DSA hint count out of range, wrong length, SLH-DSA wrong length) | Reject cost per scheme as p50 and p99 | A number per scheme, so a flood budget can use the cheapest reject, not only the full-verify reject |
| Q4 | No SIMD and no multi-thread scaling (section 8) | Build with `target-cpu=native` and with the default; run `libcrux-ml-dsa` (R11, portable and AVX2) next to `ml-dsa`; verify on 1, 2, 4, 6 threads | Throughput of verify per thread count | Scaling table; the single-core upper bound of `bandwidth-model.md` table 5 is replaced or confirmed |
| Q5 | Only 32-byte messages (section 8) | Sign and verify messages of 32, 256 and 1,024 bytes (a vote struct, a typical transaction, a larger transaction) | One row per size | Cost against message size stated |
| Q6 | No ML-KEM (section 8) | `ml-kem` (R2), `fips203` (R5), `libcrux-ml-kem` (R12): keygen, encapsulate, decapsulate, valid and invalid ciphertext; and one RLPx-shaped handshake cost: ECIES today against ECIES plus ML-KEM-768 (computed from the parts, no wire change) | Per-operation table and a handshake sum | Table plus a sum; the sum is labelled arithmetic |
| Q7 | SLH-DSA sets not measured (section 8) | SLH-DSA-192s, -256s, -256f and the SHA2 instances of R6; RustCrypto `slh-dsa 0.1.0` (R3a) | Rows as for SHAKE-128 | Complete FIPS 205 table, or a stated reason a row is skipped |
| Q8 | No memory or stack figure (section 8) | Peak RSS and stack depth per operation (`/usr/bin/time -v` per process, and the smallest thread stack that still passes) | RSS and stack per scheme | A figure per scheme, recorded with the three memory definitions of `docs/perf.md` |
| Q9 | No library run against NIST vectors (`implementations.md` section 8) | Run NIST ACVP vectors for ML-DSA, ML-KEM, SLH-DSA through each library in the lab | Pass or fail per library and parameter set | A column "ACVP vectors: pass, n of n" in the register; a fail marks the library lab-only |
| Q10 | No constant-time test (`implementations.md` section 8) | Statistical timing test (dudect-style) on sign and decapsulate for R1, R2, R4, R5, R6, R11, R12, with the threshold and sample count written down | t-statistic per function | Reported as "no leak detected at n samples" or "leak detected"; never as "constant time" |
| Q11 | No network (bandwidth model section 4) | Not in this lab. Needs a multi-client devnet and the track B proposals (#19, #20); listed so the gap stays visible | none here | none here |

Q9 and Q10 test the libraries, not the schemes. They settle whether a library may be used in a later lab step, not whether a scheme is acceptable.

## 3. Pass conditions for the lab as a whole

1. Every run record has every field of `docs/perf.md` section 2 (hardware tier label, CPU and ISA flags, SMT, frequency governor, memory, OS, build flags, binary hash, source hash, load average before and after, process placement).
2. Three runs per setup, each from a fresh process; the spread is reported; rows over tolerance are void and re-run, not reported.
3. Raw per-sample CSV kept; percentiles from `labs/pq/model.py`, cross-checked with `bench/collect/summarise.py` (the 2026-11-11 run did this for 153 rows).
4. Every call asserts its expected outcome, so a row cannot time the wrong thing.
5. Lab code and its dependencies stay out of the shipping tree: `cargo test --locked -p pq-lab --test no_ship` passes before and after.
6. Every library is labelled lab input until a person has reviewed it (`implementations.md`).

## 4. Owner decisions before the run

- **Hardware tier and host.** `docs/perf.md` needs a tier label and a host that meets its rules. The 2026-11-11 host had no tier. Issue #135 sets the rule "no benchmark on a host that holds a seated validator" and records that several fleet hosts are at 98 to 99 percent disk; this lab is CPU-only but the same host rule is the safe default. Name a host or approve a cloud instance.
- **Scope.** Run all of Q1 to Q10, or only Q1, Q3, Q4, Q6 (the ones that change a bandwidth or CPU figure). Q9 and Q10 cost the most engineering and decide library eligibility.
- **Where results go.** `labs/pq/results/<date>/` as before, with a new results document in `docs/pq/`.

## 5. Re-check on 2026-11-14

Checked again, not copied from the 2026-11-11 documents.

| Item | Result on 2026-11-14 | Source |
| --- | --- | --- |
| FIPS 203 | Planning note dated 2025-11-17 present on the NIST page; no new revision | `https://csrc.nist.gov/pubs/fips/203/final` |
| FIPS 204 | Planning note dated 2026-07-31 present; no new revision | `https://csrc.nist.gov/pubs/fips/204/final` |
| FIPS 205 | No planning note found in the page text | `https://csrc.nist.gov/pubs/fips/205/final` |
| `ml-dsa` | 0.1.2, updated 2026-10-31 (same as register R1) | `https://crates.io/api/v1/crates/ml-dsa` |
| `ml-kem` | 0.3.4, updated 2026-10-31 (R2) | `https://crates.io/api/v1/crates/ml-kem` |
| `slh-dsa` | latest stable 0.1.0 (R3a, published 2024-08-18); crate last updated 2026-04-28 by pre-release 0.2.0-rc.5 (R3b) | `https://crates.io/api/v1/crates/slh-dsa` |
| `fips204`, `fips205` | 0.5.0, updated 2026-10-23 (R4, R6) | crates.io API |
| `libcrux-ml-dsa` | 0.0.11, updated 2026-10-07 (R11) | crates.io API |
| `pqcrypto-mldsa` | 0.1.2, updated 2025-08-05 (R7) | crates.io API |
| `k256` | latest stable 0.14.0 (2026-07-08); the workspace pins 0.13.4 (`Cargo.lock`) | crates.io API |

No row of `standards.md` or `implementations.md` needs a change. The NIST errata spreadsheets and RustSec were not re-downloaded; their 2026-11-11 reading stands until the owner asks for a refresh. The `k256` gap is a note for Q2: the lab uses the workspace pin on purpose so that its ECDSA row matches the shipping code.
