# PQ implementation-assurance register

Work item: [#18](https://github.com/XDCIndia/xOne/issues/18) (XA-13), post-quantum track A. Research only. No activation, no security claim implied.

**Status of every library in this file: lab input until reviewed.** Nothing here approves a library for any shipping crate, key, validator or release. The `xone-rs-pq-readiness` skill rule applies: a pure-Rust implementation, or a FIPS algorithm name, is not module validation (see `standards.md` section 7).

Access date for every web source: **2026-11-11**. Standards pins are in [`standards.md`](standards.md). Measured speed is in [`lab-results-2026-11-11.md`](lab-results-2026-11-11.md). Speed is not assurance.

## 1. Method and evidence tags

- Source of each crate: the `.crate` file from `https://static.crates.io/crates/<name>/<name>-<version>.crate`, unpacked, with its SHA-256 in section 6. The SHA-256 of the three lab crates equals the checksum in `Cargo.lock`.
- "README" means the README in that `.crate`. "CHANGELOG" means the CHANGELOG in it.
- Licence, version dates, owners, download counts: crates.io API.
- Advisories: the RustSec advisory database (`rustsec/advisory-db`, `crates/<name>/`), read on 2026-11-11 for the crates in this register and for every crate in the lab's dependency tree.
- Unsafe, asm and FFI counts: `cargo geiger` is not installed on the host. Counts are from `grep -w unsafe` over `src/` of each crate (excluding tests and benches) and from `find` over the vendored C. They count lines that contain the keyword, comments excluded. They do **not** cover dependencies. Treat them as lower-bound indicators, not a geiger report.
- "no claim" means the README and docs that were read do not say it. It does not mean the property is absent.
- Dependency count: `cargo tree -e normal --prefix none` in a scratch crate with only that dependency, unique name-and-version lines, the crate itself removed. "+N build" is the extra count with build dependencies.

## 2. Lab choice

| Role | Crate and pin | Why |
|---|---|---|
| ML-DSA, primary | RustCrypto `ml-dsa =0.1.2`, features `alloc`, `getrandom` | Pure Rust, no `unsafe` in `src/`. Owned by the RustCrypto signatures team. Stable since 0.1.0 (2026-05-17), latest release 2026-10-31. Its CHANGELOG shows active constant-time fixes. Not audited |
| ML-DSA, cross-check | `fips204 =0.5.0`, features `default-rng` and the three sets | Pure Rust, no `unsafe`. States a source-level constant-time claim with dudect measurements. One maintainer. Used to show how far two pure-Rust libraries differ on the same loop |
| SLH-DSA | `fips205 =0.5.0`, features `default-rng`, `slh-dsa-shake-128s`, `-128f`, `-192f` | Pure Rust, no `unsafe`. Only stable pure-Rust FIPS 205 crate with a release in 2026 (0.5.0 on 2026-10-23). RustCrypto `slh-dsa` is not used: its stable line is 0.1.0 of 2024-08-18 and its 0.2 line is still a release candidate (section 3, row R3) |
| ECDSA baseline | `k256 0.13.4`, the workspace pin (feature `ecdsa` only) | What `xone-primitives` uses today. Same loop |

Not in the lab: ML-KEM (the lab measures signatures), FFI libraries (R7-R10), `libcrux` (R11, R12). Reasons are in the rows. A pure-Rust choice means no C toolchain and no FFI in the lab, as the task asked.

## 3. Register: identity and supply chain

| Id | Crate and version | Standard implemented (as stated by the crate) | Licence | Maintainer and owners | Release cadence (crates.io) | Direct and transitive crates (`cargo tree -e normal`) |
|---|---|---|---|---|---|---|
| R1 | `ml-dsa 0.1.2` (2026-10-31) | FIPS 204 final. ML-DSA-44, -65, -87 | Apache-2.0 OR MIT | RustCrypto. Owners: user `tarcieri` and team `rustcrypto:signatures` | 22 versions since 2023-08-24. 0.1.0 on 2026-05-17, 0.1.1 on 2026-06-05, 0.1.2 on 2026-10-31 | 27 with default features; 21 with the lab features |
| R2 | `ml-kem 0.3.4` (2026-10-31) | FIPS 203 final. ML-KEM-512, -768, -1024 | Apache-2.0 OR MIT | RustCrypto. `tarcieri` and team `rustcrypto:kems` | 24 versions. 0.3.0 on 2026-04-28, 0.3.3 on 2026-10-17, 0.3.4 on 2026-10-31 | 21 |
| R3a | `slh-dsa 0.1.0` (2024-08-18) | "Implemented based on the FIPS-205 Standard". CHANGELOG: updated from the initial public draft to final | Apache-2.0 OR MIT | RustCrypto (`tarcieri`, `rustcrypto:signatures`) | Stable line frozen since 2024-08-18 (a 0.0.3 backport appeared in 2025). Not a recent release | 28. Resolves `signature 2.3.0-pre.7`, a pre-release, and requires `hybrid-array 0.2.0-rc.8` in its manifest |
| R3b | `slh-dsa 0.2.0-rc.5` (2026-04-28) | Same | Apache-2.0 OR MIT | Same | Pre-release only: rc.0 2025-09-13 to rc.5 2026-04-28. No stable 0.2 | 32 |
| R4 | `fips204 0.5.0` (2026-10-23) | FIPS 204 final (August 13, 2024). Three sets | MIT OR Apache-2.0 | IntegrityChain; single crates.io owner `integritychain`. SECURITY.md: "volunteers on a reasonable-effort basis" | 0.4.6 on 2024-12-22, then 0.5.0 on 2026-10-23: 22 months with no release | 22 |
| R5 | `fips203 0.5.0` (2026-10-23) | FIPS 203 final | MIT OR Apache-2.0 | Same maintainer | 0.4.3 on 2025-02-25, then 0.5.0 on 2026-10-23 | 23 |
| R6 | `fips205 0.5.0` (2026-10-23) | FIPS 205 final. All 12 sets | MIT OR Apache-2.0 | Same maintainer | 0.4.1 on 2024-12-23, then 0.5.0 on 2026-10-23 | 25 |
| R7 | `pqcrypto-mldsa 0.1.2` (2025-08-05) | Bindings to PQClean ML-DSA-44, -65, -87 (`clean`, `avx2`, `aarch64`) | MIT OR Apache-2.0 | `thomwiggers` | 0.1.0 2024-11-04, 0.1.1 2025-02-27, 0.1.2 2025-08-05. **Marked unmaintained**: RUSTSEC-2026-0166 (2026-06-04). The upstream PQClean repository is archived on GitHub (last push 2026-08-04) | 7, +7 build (`cc`, `glob`, ...) |
| R8 | `pqcrypto-mlkem 0.1.1` (2025-08-05) | Bindings to PQClean ML-KEM | MIT OR Apache-2.0 | `thomwiggers` | 0.1.0 2024-10-24, 0.1.1 2025-08-05. Unmaintained: RUSTSEC-2026-0161 | 6, +7 build |
| R9 | `pqcrypto-sphincsplus 0.7.2` (2025-08-05) | Bindings to PQClean `sphincs-*-simple` (SPHINCS+ round 3.1 names). The vendored `crypto_sign/` has only `sphincs-*-simple` directories, from `mkannwischer/sphincsplus` (per `META.yml`), and no `slh-dsa` directory. FIPS 205 Appendix A lists differences from the SPHINCS+ submission, so **not shown to implement FIPS 205** (no vectors were run) | MIT OR Apache-2.0 | `thomwiggers` | 19 versions since 2019. Unmaintained: RUSTSEC-2026-0160 | 6, +7 build |
| R10 | `oqs 0.11.0` (2025-05-01), `oqs-sys 0.11.0+liboqs-0.13.0` | Bindings to liboqs **0.13.0** (vendored). Its README (0.13.0) says its SPHINCS+ implementations track round 3, not FIPS 205. Upstream liboqs 0.16.0 (2026-07-09) lists ML-DSA from `mldsa-native` and SLH-DSA from `slhdsa-c` (tier 3, "Community"). The Rust crate has not followed: no release since 2025-05-01, though the repository had a commit on 2026-09-28 | MIT OR Apache-2.0 for the crate. liboqs: MIT in general, with third-party modules under their own licences (`liboqs/LICENSE.txt`; subfolder licences not reviewed) | Owners: `thomwiggers` and team `open-quantum-safe:liboqs-rust-release-managers` | 13 versions since 2017. Old RustSec entries RUSTSEC-2022-0045 and -0047 (SIKE, Rainbow) are patched from 0.7.2 and do not affect 0.11.0 | 6, **+31 build** (`bindgen`, `clang-sys`, `cmake`, `regex`, ...). Needs `cmake` and `libclang` on the build host |
| R11 | `libcrux-ml-dsa 0.0.11` (2026-10-07) | FIPS 204. Portable and AVX2 code | Apache-2.0 | Cryspen: `franziskuskiefer`, `keks`, team `celabshq:crates` | 19 versions since 2025-09-10. Pre-1.0 (0.0.x). Four RustSec advisories in 2026 (section 4) | 17 |
| R12 | `libcrux-ml-kem 0.0.11` (2026-10-07) | FIPS 203. Portable, NEON and AVX2 code | Apache-2.0 | Cryspen, same owners | 26 versions since 2024-07-02 | 12 |
| B1 | `k256 0.13.4` (baseline, not PQ) | secp256k1 ECDSA | Apache-2.0 OR MIT | RustCrypto | 0.14.0 exists (2026-07-08); the workspace is on 0.13.4 | 40 with the workspace features |

Excluded: `pqcrypto-dilithium 0.5.0` (2023-10-16) and `pqc_dilithium 0.2.0`. They implement pre-standard CRYSTALS-Dilithium, not FIPS 204.

## 4. Register: assurance

| Id | Independent audit (scope and date, or "none") | Constant-time and side-channel evidence | Conformance vectors | Advisories (RustSec, 2026-11-11) |
|---|---|---|---|---|
| R1 | **none**. README: "has never been independently audited!" | **No claim** in README or docs. Source comments say division and NTT use constant-time forms. History says otherwise is possible: RUSTSEC-2025-0144 (CVE-2026-22705): timing side-channel from a hardware divide in `Decompose` during signing, patched from 0.1.0-rc.3 (0.1.2 is patched). CHANGELOG 0.1.2 lists "Fixed constant time issues (#1386)" | `tests/README.md`: NIST ACVP key-gen, sig-gen, sig-ver files (commit 65370b8 of ACVP-Server). Also Wycheproof and proptests in the repository. The ACVP JSON files are not inside the published `.crate`, so this was not re-run | RUSTSEC-2025-0144, patched in 0.1.2 |
| R2 | **none**. README: "has never been independently audited!" | **No claim** in README. CHANGELOG 0.3.3: "Eliminate secret-dependent CBD table lookups". 0.3.4: "Harden reduction", "Harden equality testing", "Harden zeroization" | Wycheproof in `tests/` | none found |
| R3a, R3b | **none**. README: "has never been independently audited!" | No claim | ACVP keygen, sig-gen, sig-ver tests and known-answer tests in `tests/` (0.1.0) | none found |
| R4 | **none claimed.** No audit mentioned in README or SECURITY.md | README claim: key generation and signing "do not branch on secret data" within one attempt. Rejection sampling makes the attempt count vary. "Checked by source review and by the dudect measurements". Source-level only: "the compiler and CPU are trusted"; verify makes no constant-time claim. Dudect output not re-run here | README: NIST ACVP key-gen, sig-gen, sig-ver (incl. HashML-DSA), C2SP Wycheproof, CCTV accumulated tests. Only the key-gen file is in the `.crate` | none found |
| R5 | **none claimed** | README claim: key generation, encapsulation and decapsulation do not branch on secret data, apart from expanding the matrix from the public seed. Source-level, dudect | NIST ACVP key-gen and encap/decap | none found |
| R6 | **none claimed** | README claim: key generation and signing "never branch, loop or index memory on secret data". Source-level, dudect. Verify: no claim | NIST ACVP key-gen, sig-gen, sig-ver (incl. HashSLH-DSA) | none found |
| R7, R8, R9 | **none.** PQClean `SECURITY.md`: implementations "have not been subjected to rigorous security audits or formal security validations", "best suited for research purposes", production use "should be preceded by a detailed expert security review" | PQClean README requirement list: "No branching on secret data (dynamically checked using valgrind)" is an **unchecked** box (`[ ]`), as is "No access to secret memory locations". So no automated constant-time check is claimed. The wrapper crates add no claim | PQClean functional tests and NIST KATs (`META.yml` has KAT hashes). Not re-run | R7 RUSTSEC-2026-0166, R8 RUSTSEC-2026-0161, R9 RUSTSEC-2026-0160, plus `pqcrypto-internals` RUSTSEC-2026-0163 and `pqcrypto-traits` RUSTSEC-2026-0162. All: informational, unmaintained, no patch, upstream archived |
| R10 | **none.** liboqs README (vendored 0.13.0 and upstream 0.16.0): "WE DO NOT CURRENTLY RECOMMEND RELYING ON THIS LIBRARY IN A PRODUCTION ENVIRONMENT OR TO PROTECT ANY SENSITIVE DATA", "has not received the level of auditing and analysis that would be necessary to rely on it for high security use" | **No claim** in the vendored README. Not checked in the upstream docs folder (not vendored) | liboqs ships its own tests. Not re-run | none for 0.11.0 |
| R11 | **none stated** in the README. Not an audit: README says the portable and AVX2 code for field arithmetic, NTT and serialization is formally verified with `hax` and F*. Verification scope is a subset of the code | README: nothing on constant time beyond the `libcrux-secrets` dependency (secret-independence types). For ML-KEM the README says constant-time operations are "best-effort", validated "via inspection of the generated assembly" | ACVP feature flag present. Not re-run | **RUSTSEC-2026-0076** panic in hint decoding during verification (fixed 0.0.8), **-0077** wrong signer-response norm check in verification (0.0.8), **-0125** AVX2 verification accepts a crafted invalid signature under a maliciously generated key (0.0.9), **-0126** AVX2 did not fully reduce, notice (0.0.9). Dependencies: `libcrux-sha3` -0074 (0.0.8), -0207 and -0208 (0.0.10), `libcrux-secrets` -0212 (0.0.6; wrong constant-time select on aarch64), `libcrux-intrinsics` -0133 (0.0.4). 0.0.11 resolves `libcrux-sha3 0.0.11`, `libcrux-secrets 0.0.7`, `libcrux-intrinsics 0.0.9`: all above the patched versions |
| R12 | **none stated**. README: portable and AVX2 code, and the generic high-level code, formally verified (`hax`, F*). Status file `proofs/verification_status.md` not in the `.crate` | README: constant-time is "best-effort"; `check-secret-independence` feature type-checks secret independence at compile time for the portable path; off by default | Not re-run | none for `libcrux-ml-kem` itself; shares the `libcrux-sha3`, `-secrets`, `-intrinsics` advisories above |
| B1 | NCC Group, report linked from the README: `research.nccgroup.com/2023/08/30/public-report-entropy-rust-cryptography-review/`. README: two high severity issues were found (ECDSA and Schnorr) and "have since been corrected". The audited version is not stated in the README. Scope beyond that was not read | README: designed so secret-dependent operations run in constant time using `subtle`. "Not suitable" for processors with variable-time multiplication | Not run | See section 5: `crypto-bigint 0.5.5` |

## 5. Observation for the security owner: shipping tree, not PQ

`cargo tree -p xone -e normal -i crypto-bigint` shows `crypto-bigint 0.5.5` in the shipping binary through `k256 0.13.4` (`xone-primitives`). RustSec has **RUSTSEC-2026-0387** (2026-10-23, `crypto-bigint`): "Multiple timing variability issues in operations intended to be constant-time", patched from 0.7.6, no unaffected range listed, so 0.5.5 is not patched. Whether the 0.5.x line contains the listed branches was not checked. The shipping paths use ECDSA recovery and verification on public data; the exposure is signing with a secret key (test code, the planned signer). A `k256 0.14.0` exists (2026-07-08). Upgrading `k256` is a separate item; this lab does not change the shipping tree. The same advisory database also lists `cmov`, `ctutils` and `keccak` entries that the lab's PQ tree already satisfies (`cmov 0.5.4`, `ctutils 0.4.8`, `keccak 0.1.6` are above the patched versions).

## 6. Register: engineering

| Id | Entropy source | Zeroization of secrets | Platform support | unsafe, asm and FFI (grep, not geiger) |
|---|---|---|---|---|
| R1 | OS RNG through the `getrandom` feature (`Generate`). `sign_randomized` takes a caller `TryCryptoRng` (rand_core 0.10). The default `Signer` impl is the deterministic FIPS 204 variant | Optional feature `zeroize`, **off by default**. `SigningKey` has a `Drop`; `ZeroizeOnDrop` only with the feature. 0.1.2 wires `shake/zeroize`. The lab does not enable it (no real keys) | `no_std`. MSRV 1.87. Pure Rust with no `unsafe`, so no SIMD intrinsics: portable code only | 0 `unsafe` in 3,899 lines of `src/`. No `forbid(unsafe_code)` attribute, so it is not enforced. No asm, no FFI |
| R2 | OS RNG or caller RNG | Optional feature `zeroize` ("memory zeroing for all cryptographic secrets"), off by default | `no_std`. MSRV 1.87 | 0 `unsafe` in 2,603 lines |
| R3a | Caller `rand_core 0.6` RNG | No `zeroize` reference in `src/` (0) and no `Drop` impls | MSRV 1.75 | 0 in 3,370 lines |
| R3b | Same | Optional `zeroize` feature, 2 `Drop` impls | `no_std`, MSRV 1.85 | 0 in 3,631 lines |
| R4 | `rand_core 0.6` `OsRng` through `default-rng`; `*_with_rng` for a caller RNG; seed functions | `zeroize` is a normal dependency. Key types derive `Zeroize, ZeroizeOnDrop` (`src/types.rs`) | `no_std`, no heap allocation, MSRV 1.85. Embedded target: `default-features = false`. `default-rng` does not build on targets without a `getrandom` backend (README). C shared library and WASM demo exist in the repository | 0 `unsafe` in 4,278 lines. README: "without any unsafe code". No asm, no FFI in the crate |
| R5 | Same pattern | Same | Same | 0 in 2,620 lines |
| R6 | Same pattern. Hedged or deterministic signing is a caller parameter (`hedged`) | Same. `DummyRng` for seed-based keygen wipes itself on drop | Same | 0 in 3,802 lines |
| R7 | `PQCRYPTO_RUST_randombytes` in `pqcrypto-internals` calls `getrandom::fill`; panics over FFI if the RNG fails | None in the Rust wrapper (0 `zeroize`, 0 `Drop`). Key types are plain byte-array newtypes. The C code makes no zeroization claim in the README | Needs a C compiler. README lists `clean`, `avx2` and `aarch64` variants "if supported"; optional `neon` feature. `wasi` needs `WASI_SDK_DIR`. `no_std` wrapper | 42 `unsafe` lines in 4,467 lines of wrapper. C: about 44,000 lines for the three ML-DSA directories (clean, avx2, aarch64), 8 `.S` assembly files in the `ml-dsa-65` directory (aarch64), 0 inline `asm`. **FFI: yes** |
| R8 | Same | Same | Same | 18 `unsafe` lines in 1,268 lines. C: about 33,400 lines, 9 `.S` files in `ml-kem-768`. FFI: yes |
| R9 | Same | Same | Same | 96 `unsafe` lines in 9,081 lines. C: about 70,400 lines for the `sphincs-*` directories. FFI: yes |
| R10 | liboqs `OQS_randombytes` (inside liboqs; not inspected). The wrapper exposes the C API | None in the Rust wrapper (0 `zeroize`). `SecretKey` is a plain buffer newtype. liboqs has `OQS_MEM_cleanse` (`common.c`) | Needs `cmake`, `libclang` (bindgen) and a C toolchain. `vendored` feature is on by default and builds the bundled liboqs 0.13.0. The README says the default features pull in OpenSSL through `oqs-sys`. `non_portable` feature for CPU-specific builds. `no_std` possible only with default features off | 57 `unsafe` lines in 1,259 lines of wrapper, plus the `oqs-sys` bindgen layer. C in the vendored liboqs: about 23,000 lines `ml_dsa`, 84,000 `ml_kem`, 56,000 `sphincs`, 13,000 common; 12 `.S` files in `ml_dsa`. **FFI: yes** |
| R11 | Caller supplies randomness as bytes. README example uses the OS RNG and says that is "not recommended", prefer a NIST DRBG | No `zeroize` reference in `src/` (0) | `no_std` (default `std` feature). Portable, plus AVX2 and NEON through the `simd256` and `simd128` features. `libcrux-secrets` has aarch64 inline assembly (see RUSTSEC-2026-0212) | 35 `unsafe` lines in 13,604 lines, `#![deny(unsafe_code)]` at the root with `allow` in the AVX2 modules (`simd/avx2/ntt.rs`, `invntt.rs`, `samplex4.rs`, `instantiations/avx2.rs`). Dependencies (`libcrux-intrinsics`, `libcrux-sha3`, `libcrux-secrets`) hold more and are not counted |
| R12 | Caller supplies randomness. Optional `rand` feature | No `zeroize` reference in `src/` (0) | `no_std`, portable, NEON, AVX2 | 50 `unsafe` lines in 21,020 lines, `#![deny(unsafe_code)]` |
| B1 | `k256` signs with RFC 6979 (deterministic); no RNG | `ecdsa 0.16.9` `SigningKey` implements `ZeroizeOnDrop` | `no_std` | 0 `unsafe` (`#![forbid(unsafe_code)]`) |

## 7. Pinned crate files

SHA-256 of the `.crate` files read. Re-pinning needs a new row and a new access date.

| Crate | SHA-256 |
|---|---|
| `ml-dsa-0.1.2` | `6f6d49c3f3f0e10e18ca975ffda66f8da0ec36521c1d911adcbc521bf0743f5c` |
| `ml-kem-0.3.4` | `84ca78b66f46050d49fa21693ae8adab2081d8101b65d8067316b6b70b64a0d1` |
| `slh-dsa-0.1.0` | `069c0368c5352eeea0134fa9bfd45feb28c5c5412719b2db81c12c784adb1aef` |
| `slh-dsa-0.2.0-rc.5` | `371c02fe34044d8866ddf7cb0e8204a87ef31a39f0408bed41c4253ea9dd61ed` |
| `fips204-0.5.0` | `a823e139c2d539b1b735d31cdce8b0b356671585eac3d54dec79d1d15052093b` |
| `fips203-0.5.0` | `602e37d80ffd53e1b6037a91bbc471d94bd18a6d1f86493b721b7ef4609a013c` |
| `fips205-0.5.0` | `4a5a5dcc490a93ad15deabf0534af8b2b6e9d20e01e8066f6fcba4da7e316100` |
| `pqcrypto-mldsa-0.1.2` | `f9f812cd126a2582599478a434fea75937b4b05d234c64a49e0cea129e130528` |
| `pqcrypto-mlkem-0.1.1` | `eb14d207f3749e8a59a026c22ceaa72d70fff931cfbf4c8d9b08f3fc56dc6e60` |
| `pqcrypto-sphincsplus-0.7.2` | `4f6ff8925443869aab1332bb8b0fe3b75cf113516bccf05da4dc71bc33162252` |
| `pqcrypto-dilithium-0.5.0` | `685de0fa68c6786559d5fcdaa414f0cd68ef3f5d162f61823bd7424cd276726f` |
| `oqs-0.11.0` | `48caac02cf42ba00b865a747e332828a75341d97ae35ad1ae9785e56de212e78` |
| `oqs-sys-0.11.0+liboqs-0.13.0` | `ac6d66ee528a895ce5cc08851698d109c5d7ee5d7a0b3b40d61550eda91e414f` |
| `libcrux-ml-dsa-0.0.11` | `31f8706f014055bed7b2706efb8e646e4590d95e447dfc146be2ed4705920dda` |
| `libcrux-ml-kem-0.0.11` | `4ceed00367748ec0b545bc4a7b0521f8c9fd476d02484340f0a95b0f45f828cd` |

## 8. What this register does not establish

- No library was reviewed by a person. No library was run against NIST vectors by this work.
- No constant-time test was run. Claims above are the crates' own statements or advisories.
- Counts of `unsafe` do not include dependencies and are not a `cargo geiger` report.
- RustSec was read as of 2026-11-11. An empty result means no entry in that database, not that no bug exists.
- Not covered: `mldsa-native` and `slhdsa-c` (the upstream C code liboqs 0.16.0 now uses), other languages, hardware modules, and any library not on crates.io.

## 9. Sources

- crates.io API: `https://crates.io/api/v1/crates/<name>`, `/versions`, `/owners`.
- RustSec advisory database: `https://github.com/rustsec/advisory-db/tree/main/crates/<name>` (entries named above).
- liboqs 0.16.0 README: `https://github.com/open-quantum-safe/liboqs/blob/main/README.md`; release list `https://github.com/open-quantum-safe/liboqs/releases` (0.16.0 on 2026-07-09). liboqs-rust: `https://github.com/open-quantum-safe/liboqs-rust`.
- PQClean archived status: `https://github.com/PQClean/PQClean` (archived flag, last push 2026-08-04); RustSec text cites `PQClean/PQClean#604`.
- NCC Group, Entropy Rust Cryptography Review (linked from the `k256` README): `https://research.nccgroup.com/2023/08/30/public-report-entropy-rust-cryptography-review/` (not read in full).
