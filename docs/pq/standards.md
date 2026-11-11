# PQ standards pins

Work item: [#18](https://github.com/XDCIndia/xOne/issues/18) (XA-13), post-quantum track A. Research only. No activation, no security claim implied. Selecting a scheme for the protocol is a non-goal.

Wording rule. The NIST algorithms below are called "quantum-resistant under stated assumptions". They are never called "quantum proof". NIST itself says ML-KEM "is believed to be secure, even against adversaries who possess a quantum computer" [S1]. A FIPS algorithm name is not a validated module (see section 7).

Access date for every NIST page and file in this document: **2026-11-11**. Every PDF and spreadsheet was downloaded on that date and its SHA-256 is in section 9. Re-check each page whenever the pin is used. The errata spreadsheets say they list potential updates, not official changes.

## 1. Pin table

| Standard | Revision pinned | Status at access | Errata and notices at access | Parameter sets |
|---|---|---|---|---|
| FIPS 203, ML-KEM [S1] | Final, published 2024-08-13 (draft 2023-08-24) | Final. No revision after 2024-08-13 | Planning note dated 2025-11-17: an issue "will be corrected in a future update/revision". Errata spreadsheet: 2 entries (section 2) | ML-KEM-512, -768, -1024 |
| FIPS 204, ML-DSA [S2] | Final, published 2024-08-13 (draft 2023-08-24) | Final. No revision after 2024-08-13 | Planning note dated 2026-07-31: "several minor issues that will be corrected in a future update/revision". A note of 2025-01-31 points to the PQC FIPS FAQ. Errata spreadsheet (last updated 2026-07-31): 12 entries (section 2) | ML-DSA-44, -65, -87 |
| FIPS 205, SLH-DSA [S3] | Final, published 2024-08-13 (draft 2023-08-24) | Final. No revision after 2024-08-13 | None on the page: no planning note, no errata link. Related: SP 800-230 initial public draft (section 4) | 12 sets: SHA2 and SHAKE at 128, 192, 256 bits, each s and f |
| SP 800-208, LMS/HSS and XMSS/XMSS^MT [S4] | Final, published October 2020 (final 2020-10-29, draft 2019-12-11) | Final | None on the page | LMS, HSS, XMSS, XMSS^MT (stateful). Only needed if stateful signatures are considered for release signing |
| NIST IR 8547, transition [S5] | Final, published October 2026 (final 2026-10-27, draft 2024-11-12) | Final | None on the page | Not a scheme standard. Timeline in section 6 |

Pin rule. A document that names a standard says "FIPS 204 (2024-08-13), errata as of 2026-11-11" and cites this file. Changing a pin needs a new access date and a re-read of the errata.

## 2. Errata status (checked, not copied)

The counts in `threat-model.md` were re-checked by downloading both spreadsheets and reading every row. They are correct. Dates below are Excel serial dates converted to ISO.

**FIPS 203: 2 entries.** The sheet says corrections "DO NOT introduce new technical requirements" and "ARE NOT official changes".

| Identified | Location | Issue (short) |
|---|---|---|
| 2025-03-31 | Appendix A | Algorithms 9 and 10 use zeta for i = 1..127. Appendix A also lists the value 1 (i = 0). A clarifying sentence will be added |
| 2025-10-17 | Section 5.3, Algorithm 15, line 7 | Comment says "decode plaintext m from polynomial v". `v` is not used. It will read `w` |

**FIPS 204: 12 entries**, identified 2024-09-20 to 2026-07-31. Same wording about official changes.

| Identified | Location | Issue (short) |
|---|---|---|
| 2024-09-20 | Sec. 2.5, 7.5 | NTT text evaluates the polynomial `w` twice. It should be once |
| 2024-10-17 | Sec. 6.2, 6.3 | `M` should read `M'` (message after the context prefix) in two sentences |
| 2025-12-02 | Various | Broken intra-document links |
| 2025-12-02 | Sec. 2 | `AES` defined, never used. Some vectors not bold |
| 2025-12-02 | Sec. 3 | Footnote 2 notation. In Sec. 3.3 `c~` should be `mu \|\| w1`, as in the algorithms |
| 2025-12-02 | Sec. 5 | Algorithms 1, 2, 4 use "NULL" for RNG failure. Will be the bottom symbol |
| 2025-12-02 | Sec. 6 | "polynomial" should be "polynomial vector" in places. Pseudocode to use Power2Round |
| 2025-12-02 | Sec. 7 | Text above Algorithm 28 names the wrong callers of `w1Encode` |
| 2025-12-02 | App. A | Montgomery reduction text differs from the reference implementation. Appendix A will be rewritten to match it |
| 2026-02-23 | Sec. 5.3, Algorithm 3, step 2 | Return "false" instead of the error symbol |
| 2026-02-27 | Sec. 7.4, Algorithm 40 (`UseHint`) | Output bound should be `0 <= r1 <= (q-1)/(2 gamma2) - 1` |
| 2026-07-31 | Sec. 4, App. C | Repetition counts in Table 1 are not exact. They will read 4.36, 5.14 and 3.91 (now 4.25, 5.1, 3.85). The limit on loop iterations is also affected |

Consequence for this lab. The entries correct text and tables. None changes a public-key size or a signature size. The lab checks sizes from the libraries against Table 2 (section 3). The lab does not run NIST vectors, so it makes no claim that a library follows the errata. Each library's own vector statement is in `implementations.md`.

**FIPS 205: none.** The page lists two history entries (draft 2023-08-24, final 2024-08-13) and no notice.

**SP 800-208: none** on the page at access.

## 3. Parameter sets and sizes

Sizes are in bytes. All are from the FIPS PDFs read on 2026-11-11 (page numbers are PDF pages).

### ML-DSA (FIPS 204, Table 1 and Table 2, PDF pages 25-26)

| Set | Category | (k, l) | eta | tau | gamma1 | gamma2 | omega | Private key | Public key | Signature |
|---|---|---|---|---|---|---|---|---|---|---|
| ML-DSA-44 | 2 | (4,4) | 2 | 39 | 2^17 | (q-1)/88 | 80 | 2,560 | 1,312 | 2,420 |
| ML-DSA-65 | 3 | (6,5) | 4 | 49 | 2^19 | (q-1)/32 | 55 | 4,032 | 1,952 | 3,309 |
| ML-DSA-87 | 5 | (8,7) | 2 | 60 | 2^19 | (q-1)/32 | 75 | 4,896 | 2,592 | 4,627 |

q = 8,380,417 for all sets. Signing is a rejection loop; the expected repetitions in Table 1 are 4.25, 5.1 and 3.85 (to be 4.36, 5.14, 3.91 per the errata). So signing time varies by design. Signing takes a context string of at most 255 bytes. The seed `xi` (32 bytes) may be stored in place of the expanded private key: the NIST PQC FIPS FAQ (last revised 2026-06-16) allows this [S6]. IR 8547 Table 3 notes that ML-DSA-44 drops to category 1 if the approved RBG has less than 192 bits of security [S5].

### SLH-DSA (FIPS 205, Table 2, PDF page 53)

| Set (SHA2 or SHAKE) | Category | n | h | d | Public key | Signature |
|---|---|---|---|---|---|---|
| 128s | 1 | 16 | 63 | 7 | 32 | 7,856 |
| 128f | 1 | 16 | 66 | 22 | 32 | 17,088 |
| 192s | 3 | 24 | 63 | 7 | 48 | 16,224 |
| 192f | 3 | 24 | 66 | 22 | 48 | 35,664 |
| 256s | 5 | 32 | 64 | 8 | 64 | 29,792 |
| 256f | 5 | 32 | 68 | 17 | 64 | 49,856 |

Private key is 4n bytes. Each set is designed for at most 2^64 signatures per key. "s" means small signature and slow signing. "f" means fast signing and larger signature. SLH-DSA is stateless: no signature counter has to survive a restore. FIPS 205 is based on SPHINCS+ and "contains several minor modifications compared to Version 3" of the submission; Appendix A (PDF p. 61) lists them. A library that ships the submission variants (see `implementations.md`, R9 and R10) is not shown to implement FIPS 205.

### ML-KEM (FIPS 203, Table 1 PDF page 24, Tables 2 and 3 PDF page 48)

| Set | Category | k | Encapsulation key | Decapsulation key | Ciphertext | Shared secret | Decapsulation failure rate |
|---|---|---|---|---|---|---|---|
| ML-KEM-512 | 1 | 2 | 800 | 1,632 | 768 | 32 | 2^-138.8 |
| ML-KEM-768 | 3 | 3 | 1,184 | 2,400 | 1,088 | 32 | 2^-164.8 |
| ML-KEM-1024 | 5 | 4 | 1,568 | 3,168 | 1,568 | 32 | 2^-174.8 |

ML-KEM is a key-encapsulation mechanism. It does not sign and does not authenticate a peer. IR 8547 Table 5 says NIST recommends ML-KEM-768 as the default set [S5]. It is not used in the lab: the lab measures signatures only.

## 4. SP 800-230 (draft), related to FIPS 205

NIST SP 800-230, "Additional SLH-DSA Parameter Sets for Limited-Signature Use Cases", initial public draft dated 2026-04-13 [S7]. Comments closed 2026-06-12; the page notes that the public comments are available (planning note 2026-07-01). It specifies six more parameter sets for categories 1, 3 and 5 with a limit of 2^24 signatures per key, "not approved for general-purpose use", aimed at software, firmware and certificate signing. It is a draft. It is not a pin for any use. It is listed because release signing (#20) may look at it.

## 5. SP 800-208: stateful hash-based signatures

Only for the case that a stateful scheme is considered for release or snapshot signing (#20).

- The standard approves LMS and HSS, and XMSS and XMSS^MT. Published October 2020 [S4].
- Section 1.2 (PDF p. 10): an attacker who gets two signatures made with the same one-time key can forge signatures. The signer must "ensure that no individual OTS key is ever used to sign more than one message".
- Section 8.1 (PDF p. 37): implementations of key generation and signing "shall only be validated for use within hardware cryptographic modules" with FIPS 140-2 or 140-3 Level 3 or higher physical security. The module must not export private key material. Before it returns a signature it must increment the leaf index and store it in non-volatile storage [S4].
- Consequence. A software-only lab implementation of LMS or XMSS cannot be "SP 800-208 conformant". A datadir rollback or a restored backup of a signing host must never restore the leaf index (skill `xone-rs-durability-signer-gate`). No stateful scheme is benchmarked here.
- IR 8547 Table 3 lists LMS/HSS and XMSS/XMSS^MT at categories 3 (192-bit variants) and 5 [S5].

## 6. IR 8547 transition timeline

NIST IR 8547, "Transition to Post-Quantum Cryptography Standards", final, October 2026 (page: final 2026-10-27) [S5]. Section and table numbers are from the PDF.

| Item | Text in IR 8547 | Where |
|---|---|---|
| Quantum-vulnerable signatures | ECDSA and EdDSA (FIPS 186) and RSA: ECDSA at 112 bits or more classical strength, EdDSA at 128 bits or more, RSA at 112 bits or more. "Deprecated after 2030. Disallowed after 2035" | Table 2, PDF p. 22 |
| Quantum-vulnerable key establishment | Finite-field DH and MQV, elliptic-curve DH and MQV (SP 800-56A) at 112 bits or more: "Deprecated after 2030. Disallowed after 2035" | Table 4, PDF pp. 23-24 |
| Meaning | NIST "will deprecate quantum-vulnerable public-key cryptography after 2030 and disallow its use under NIST standards and guidelines after 2035". The window is for lower-risk systems. It does not change earlier federal deadlines | Section 4 intro, PDF p. 20 |
| Federal high-value and high-impact systems | Post-quantum key establishment by 2030-12-31 and digital signatures by 2031-12-31, under Executive Order 14412 and OMB Memorandum M-26-15 | Introduction and Section 4, PDF pp. 8 and 20 |
| Federal overall goal | National Security Memorandum 10: migration complete by 2035 | Section 4, PDF p. 20 |
| Approved PQ signatures | ML-DSA (FIPS 204), SLH-DSA (FIPS 205), LMS/HSS and XMSS/XMSS^MT (SP 800-208) | Table 3, PDF p. 23 |
| Approved PQ key establishment | ML-KEM (FIPS 203). HQC is selected, its FIPS is "under development" | Table 5, PDF p. 24 |
| Not yet a standard | A FIPS for a FALCON-derived signature is "under development" (not in Table 3) | PDF p. 11 |
| Hybrids | Hybrid solutions "may initially" be used. They add complexity. NIST "leaves the decision to each specific application" | Section 3.2, PDF pp. 17-18 |
| Security categories | 1: AES-128 key search. 2: collision search on SHA-256. 3: AES-192. 4: collision search on SHA3-384. 5: AES-256 | Table 1, PDF p. 22 |

Reading for XDC. These are NIST's dates for NIST standards and for federal systems. XDC is not bound by them. They tell when a deployed ECDSA-based product may stop being acceptable to NIST-following customers and auditors. They set no date for any XDC rule change; that decision belongs to Track B and governance.

## 7. What "FIPS" means for the libraries

- A FIPS number names an algorithm specification. A crate that says it "implements FIPS 204" has not thereby been validated. Algorithm conformance testing (CAVP) and module validation (CMVP, FIPS 140-3) are separate programs. The NIST PQC FAQ speaks of "CAVP testing" for the seed-based key formats [S6].
- No crate in the register states a CMVP certificate in the README and docs that were read. SP 800-208 section 8.1 (above) makes module validation a hard requirement for LMS and XMSS.
- A pure-Rust implementation is not module validation either.

## 8. Open question Q1: truncated addresses

`threat-model.md` asks #18 to source any statement about the 160-bit address (`keccak256(pubkey)[12:]`) before making it. What can be said from primary sources, and what cannot:

- NIST defines post-quantum security by reference primitives: key search on AES-128 for category 1, collision search on SHA-256 for category 2 (IR 8547 Table 1) [S5]. NIST assigns categories to full-length standardized hashes (Table 8). It does not assign one to a 160-bit truncation of Keccak-256. No NIST statement about a 160-bit truncated address was found.
- Arithmetic, not a finding. A generic search for a preimage of a 160-bit value takes 2^160 evaluations classically. Grover's algorithm needs about the square root of the search space, so 2^80 sequential iterations [S8]. A generic classical collision search on 160 bits takes about 2^80 evaluations (birthday bound). The quantum collision algorithm of Brassard, Hoyer and Tapp gives a cube-root speed-up in query count [S9]; its practical cost depends on memory assumptions that are disputed. These are query counts. They are not costs of a real machine and they do not include the cost of a Keccak circuit.
- Not answered here: the practical cost of a 2^80-iteration quantum search, and the effect on an address that has never been used to sign (its public key is hidden behind the hash). Track B must not state a security level for the 160-bit address until a primary source for that cost is added here.

## 9. Sources

All accessed 2026-11-11.

- [S1] NIST, FIPS 203 (final): https://csrc.nist.gov/pubs/fips/203/final (history, planning note 2025-11-17, abstract). PDF: https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.203.pdf (Tables 1-3). Errata: https://csrc.nist.gov/files/pubs/fips/203/final/docs/fips-203-potential-updates.xlsx
- [S2] NIST, FIPS 204 (final): https://csrc.nist.gov/pubs/fips/204/final (history, planning notes 2026-07-31 and 2025-01-31). PDF: https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.204.pdf (Tables 1-2). Errata: https://csrc.nist.gov/files/pubs/fips/204/final/docs/fips-204-potential-updates.xlsx
- [S3] NIST, FIPS 205 (final): https://csrc.nist.gov/pubs/fips/205/final. PDF: https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.205.pdf (Table 2).
- [S4] NIST, SP 800-208: https://csrc.nist.gov/pubs/sp/800/208/final. PDF: https://nvlpubs.nist.gov/nistpubs/SpecialPublications/NIST.SP.800-208.pdf (Sections 1.2 and 8.1).
- [S5] NIST, IR 8547 (final): https://csrc.nist.gov/pubs/ir/8547/final. PDF: https://nvlpubs.nist.gov/nistpubs/ir/2026/NIST.IR.8547.pdf
- [S6] NIST, PQC FAQs (seed as key format, last revised 2026-06-16): https://csrc.nist.gov/Projects/post-quantum-cryptography/faqs
- [S7] NIST, SP 800-230 (initial public draft): https://csrc.nist.gov/pubs/sp/800/230/ipd
- [S8] L. K. Grover, "A fast quantum mechanical algorithm for database search", https://arxiv.org/abs/quant-ph/9605043
- [S9] G. Brassard, P. Hoyer, A. Tapp, "Quantum Algorithm for the Collision Problem", https://arxiv.org/abs/quant-ph/9705002

SHA-256 of the files read (for re-pinning):

| File | SHA-256 |
|---|---|
| nist.fips.203.pdf | `fe1f12f32a7e44ec9fdebbf400cda843a40b506dee676725234dc6f7923b6cac` |
| nist.fips.204.pdf | `57239b9f84c03227eda3ca0991204dc7764c79af9ce2e6824eda774918d46b6b` |
| nist.fips.205.pdf | `8ef34228276f3386d23cb0da8c14592b8cfb0db3358016bba64df7a004f8d13d` |
| NIST.SP.800-208.pdf | `9ac6bee3da878e883874847b40f80d2265d6268385d8201f866581437533465f` |
| NIST.IR.8547.pdf | `3049cdfdd7c20417cf588a3d55ace6e8813982d1499502b7ef69e9593b2dc280` |
| fips-203-potential-updates.xlsx | `edf899c89762449f43d7713883caeefc2e4ae9ae98d5a76b339547db22cb3ac7` |
| fips-204-potential-updates.xlsx | `5bc93ce63bc647e6d1d456cb2d3a171426c15aca4a7a0e0edd40d08b7a34c793` |
