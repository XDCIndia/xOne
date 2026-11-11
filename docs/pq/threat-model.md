# Post-quantum threat model for XDC and the planned xOne client

Work item: [#17](https://github.com/XDCIndia/xOne/issues/17) (XA-12), track A. Companion to [`inventory.md`](inventory.md). Research only: no activation, no security claim implied. Row IDs (`T01`, `P05`, ...) refer to the inventory. geth-XDC pin `6fc8e8c2088b7f18264f5663036669d04fa7becf`. All external sources were accessed on 2026-11-11 (section 10).

Wording rule. This document says "quantum-resistant under stated assumptions" for the NIST post-quantum algorithms. It never says "quantum proof". NIST's own FIPS 203 abstract uses "believed to be secure, even against adversaries who possess a quantum computer" [S1b].

## 1. Attacker model and terms

- **CRQC**: a cryptographically relevant quantum computer, one able to solve the discrete logarithm problem on secp256k1 (and the other curves in the inventory). No such machine is assumed to exist. This document asks what is already exposed on chain if one appears later, and what must be decided before that.
- **Resource estimate, for scale only.** Roetteler, Naehrig, Svore and Lauter give a circuit for elliptic-curve discrete logarithms over an n-bit prime field using `9n + 2*ceil(log2 n) + 10` qubits and `448*n^3*log2(n) + 4090*n^3` Toffoli gates [S6]. Substituting n = 256 (my arithmetic, not a figure from the paper): 2,330 qubits and about 1.29 x 10^11 Toffoli gates, in the paper's logical-qubit model. This is not a statement about any machine that exists, and it excludes the error-correction overhead a real device needs.
- **Harvest-now-forge-later (HNFL)**: an attacker records public data today (signatures, public keys) and uses it after a CRQC exists to forge signatures or take over keys. For ECDSA this is mostly about key exposure: the public key, recoverable from any signature, is all Shor-type attacks need.
- **Harvest-now-decrypt-later (HNDL)**: the same recording strategy against confidentiality.
- **One-shot versus replayable**: a signature is *one-shot* if the protocol makes it useless after first use (nonce consumed, round passed). It is *replayable* if a recorded copy stays valid somewhere. This matters less than key exposure for ECDSA, because a CRQC attacker does not need to replay: they sign fresh messages with the recovered key. The distinction is kept in section 3 because it decides which signatures need protection at the instant of signing (real-time forgery) and which only need the key to be retired.
- Symmetric primitives and hashes (AES, Keccak, SHA-256, scrypt) are out of the main analysis. NIST IR 8547 Table 1 lists AES-128 as security category 1, SHA-256 collision search as 2, AES-256 as 5 [S5]. This document does not analyse Grover-type attacks. The 160-bit address truncation (`keccak256(pubkey)[12:]`) is an open question (section 9, Q1), not a finding.

## 2. Standards state (pins and errata, accessed 2026-11-11)

| Standard | Status on NIST page | Published | Errata and notices at access | Use here |
|---|---|---|---|---|
| FIPS 203, ML-KEM [S1] | Final. History: draft 2023-08-24, final 2024-08-13 | 2024-08-13 | Planning note dated 2025-11-17: "We've identified an issue that will be corrected in a future update/revision of this publication." The linked spreadsheet "Errata (potential updates)" (SHA-256 `edf899c8...22cb3ac7`) has 2 entries, identified 2025-03-31 (Appendix A, a clarifying sentence about the zeta table) and 2025-10-17 (Section 5.3, Algorithm 15 comment: `v` should read `w`). The spreadsheet states that corrections are "NOT official changes" and "DO NOT introduce new technical requirements" | Key establishment only (a KEM). Not a signature and not identity authentication |
| FIPS 204, ML-DSA [S2] | Final. History: draft 2023-08-24, final 2024-08-13 | 2024-08-13 | Planning note dated 2026-07-31 pointing to "Errata (potential updates)" (SHA-256 `5bc93ce6...b7a34c793`): 12 entries identified between 2024-09-20 and 2026-07-31, among them Sec. 2.5 and 7.5 NTT text, Sec. 6.2 and 6.3 (`M` to `M'`), Algorithm 3 step 2 (return "false" instead of the error symbol), Algorithm 40 `UseHint` bound, and Table 1 repetition counts (2026-07-31). Same "NOT official changes" wording. A planning note of 2025-01-31 points to a PQC FIPS FAQ | Candidate account, validator and infrastructure signatures. Sizes: ML-DSA-65 private key 4,032 B, public key 1,952 B, signature 3,309 B; ML-DSA-44 1,312 B public key, 2,420 B signature; ML-DSA-87 2,592 B and 4,627 B (Table 2, PDF page 26 of the PDF I read, SHA-256 `57239b9f...18d46b6b`). Signing takes a context string of at most 255 bytes (Algorithm 2 text), usable as a domain tag |
| FIPS 205, SLH-DSA [S3] | Final. History: draft 2023-08-24, final 2024-08-13 | 2024-08-13 | None on the page ("no errata, update notice, or change-log entries beyond the two document history entries") | Stateless hash-based signature candidate. Size and cost not measured here (#18) |
| NIST SP 800-208 [S4] | Final, 2020-10-29 (draft 2019-12-11) | October 2020 | None listed | Stateful hash-based signatures LMS and XMSS (and multi-tree HSS, multi-tree XMSS). A stateful scheme needs a durable one-time-state reservation that survives restore and failover; see `xone-rs-pq-readiness` rule 6 and #12 |
| NIST IR 8547, transition [S5] | Final, 2026-10-27 (draft 2024-11-12; the draft page says it was obsoleted by the final) | October 2026 | n/a | Table 2: ECDSA and EdDSA (FIPS 186) and RSA, at 112 and 128 bits of classical strength, "Deprecated after 2030, Disallowed after 2035". Table 4: ECDH and MQV and finite-field DH, same dates. Section 3.2: hybrid solutions "typically designed to remain secure if at least one of the component algorithms is secure"; NIST "leaves the decision to each specific application" |

Pin rule (from `xone-rs-pq-readiness`): re-check these pages at each use; the errata spreadsheets are marked as potential, not official, updates. Nothing here selects a parameter set.

## 3. Harvest-now-forge-later, per signature class

"Window" is the period in which the attacker's action is useful.

| Class (inventory rows) | What an observer can already harvest | Replayable or one-shot | What a key break gives | The window that matters |
|---|---|---|---|---|
| Transaction signatures, all envelopes (T01-T08) | Every signature in every mined block, hence every public key of every account that ever sent a transaction | The signed transaction is one-shot on its chain (nonce, chain ID). Exception: a pre-EIP-155 transaction (T02) carries no chain ID and is valid on any chain where that address has that nonce | Full control of the account: new transactions with any content, at any time | Permanent for every exposed account. A second, short window applies to a first-ever transaction: its key is public only from the moment it enters the mempool until it is mined, which is the only time a real-time attacker could front-run it. XDC block time and the CRQC solve time decide whether that is reachable; not assessed |
| V1 seal M1 and double validation M2 (T09, T10) | Every V1 header carries both signatures; every V1-era masternode key is public | One-shot per block (digest covers parent, number, content). M1 and M2 sign the same digest | Forge a V1 block as that masternode, or its validator co-signature | Only for networks that still run V1 rules. For history: forged V1 headers are only believable to nodes that cannot check against a trusted anchor |
| V2 block proposer signature (T11) | Every V2 header | One-shot per block | Propose blocks as that masternode in its turns | While the key is in an epoch's masternode set, and for history as above |
| V2 vote and QC (T12, T14) | Every vote on the wire, and in every V2 header the parent QC with up to 78 signatures in the Apothem sample (inventory section C) | A vote is bound to `{block hash, round, number, gap}`: useless elsewhere. A recorded QC is replayable only as the exact same certificate | With 2/3 of an epoch's keys (at least 73 signatures at 108 masternodes, `utils.go:365-376`): a valid QC for any block, hence a forged finality proof | Any epoch whose masternode keys are exposed (all of them, by T09-T12). Permanent for history |
| V2 timeout and TC (T13, T15) | Every timeout message and TC | Bound to `{round, gap}` only; **not** chain specific (section 7) | A TC for any round | Permanent; plus cross-network replay today, without any quantum capability (section 7) |
| SyncInfo (T16) | Carries a QC and a TC | Not signed itself | Nothing beyond T14 and T15 | n/a |
| Masternode sign transaction to 0x89 (T17) | Every sign tx; reward weights are computed from senders (`reward.go:77-90`) | One-shot (nonce); the contract also bounds the block number to `epochNumber * 2` of the head (`legacypool.go:849-897`) | Forge sign transactions from the masternode account, which skews reward counts | Permanent key exposure; practical effect needs the transaction to be included or accepted into the pool on the special-tx path (`legacypool.go:978`) |
| Randomize secret and opening to 0x90 (T18) | Ciphertexts in transactions, later the opening key | Secret and opening are public after reveal. The transaction signature is one-shot | The AES-CFB commit-reveal is symmetric. A masternode key break lets an attacker submit the masternode's setSecret or setOpening | No emitter at the pin (inventory T18); relevant only for replay of V1 history |
| RLPx handshake (T26) | Recorded handshakes with the static public key (public by design, T28) | The auth message is bound to a fresh nonce and ephemeral key: not replayable | Section 6 | See section 6 |
| discv4, ENR, DNS tree (T28, T29, T45) | All packets and records | discv4: bound to type and content, no timestamp beyond expiry fields in the body. A DNS root signs `{roots, seq}` with no expiry (`tree.go:278-281`), so an old signed root is a replay candidate unless clients track `seq` (unknown) | Forge records and tree roots as that node or list owner | Permanent key exposure. Impact is eclipse or redirection, not theft |
| `eth_sign`, `personal_sign`, EIP-712 (T47) | Signatures handed to contracts and dapps | Depends on the verifying contract: a permit with no nonce is replayable | As an account key (T01) | Contract specific; not enumerable from the client |
| Release signatures (T42, P08) | Public `.sig` files, if the pipeline publishes them (unknown) | Replayable by nature: a signature over an old artifact stays valid forever | Sign malicious artifacts, including old version numbers (downgrade) | The lifetime of the verification key, extended by every user who pins it |
| Precompile `ecrecover` verifiers (T32) | Whatever on-chain signatures contracts accept | Contract specific | Everything those contracts guard | Contract specific. Bridges and oracles using it are not enumerable from the client (T44) |

## 4. Exposed and reused keys

1. **Every masternode key is exposed at every seal.** Each header the node produces carries a signature from which the public key is recovered (T09, T11). There is no hidden-until-use window for an active masternode.
2. **One key signs everything.** The same secp256k1 key signs V1 seals and validations, V2 proposals, votes and timeouts, and the sign and randomize transactions (T09-T13, T17, T18). It is held unlocked in the node process (T22, `eth/backend.go:783-817`). A single discrete-log break therefore transfers all of a masternode's consensus authority at once. There is no key separation by role in the protocol, and the seal digest is shared between creator and validator (T10).
3. **Old keys stay dangerous.** Historical epochs' masternode keys are exposed in block history, and nothing retires them. After a CRQC, the set of keys that were once a 2/3 majority of an epoch is enough to build QC-valid history from that point (section 3, QC row). A node that syncs from genesis or from an anchor older than the forgery point, with no PQ-authenticated checkpoint, cannot tell a forged continuation from the real one by signatures alone. This is the main reason #14 (authenticated anchor) and #20 (checkpoints) matter.
4. **Contract owners.** The masternode voting contract (0x88) is governed by transaction signatures from candidate owners and voters (T19). The owner key (stake, resign, withdraw) can be separate from the masternode key, and then is exposed only when its owner transacts. Voters and owners expose their keys at first use like any account. The exact access-control logic is not in the pinned tree (inventory T19), so owner-path abuse is not analysed beyond "whoever holds the owner key controls that candidate's stake". The foundation and team addresses (T20) and any upgradeable contract admin (T21) are unknown: an admin key on a system or application contract is a single exposed key with the power to change code.
5. **Node identity keys (T28, T46)** are public by design and stored unencrypted in `nodekey` (mode 0600). Reuse of the node key as a masternode key is not observable from the client.

## 5. Legacy balances and account migration

- **What is exposed.** Every externally owned account that has ever sent a transaction has an exposed public key (T01). The inventory cannot say how much value those accounts hold: the client does not index it. Measurement owed to #18: from an archive state at a pinned block, sum balances of addresses that have signed at least one transaction, and report the fraction of total supply (method only; no number is claimed here).
- **A PQ verifier does not protect an account that a legacy path can still move.** There is no mechanism in the client at the pin to disable ECDSA for an address (`recoverPlain`, `transaction_signing.go:490-515`, accepts any valid signature for the address, independent of the account's code). EIP-7702 delegation (T07, T08) installs code at an EOA but the authority is still recovered with `Ecrecover` (T08), and sender recovery for ordinary transactions is unchanged (T01), so a delegating account's ECDSA key remains able to originate transactions and to change its delegation (inferred from the code paths cited; not run). A smart-account PQ verifier layered over an exposed EOA key is bypassable by whoever holds the broken key.
- **Contracts that verify ECDSA** (T32, T47) keep trusting it after any chain-level change. Their state is outside client control.
- **Consequence.** Protecting balances needs a protocol-level rule for exposed-key accounts (migration deadline, opt-in freeze, time-lock, or none), a PQ-capable account type that has no ECDSA fallback, and a decision for users who cannot migrate in time. That is a governance decision for Track B (#19), not an interface decision.
- **Never-spent addresses** show only a Keccak-based hash of the key until first use. Their protection is bounded by hash search cost, which this document does not quantify (Q1).
- **XDCx privacy transactions (T41)** used ring signatures and range proofs on secp256k1 for blocks before `TIPXDCXMinerDisable`. Their hiding properties rest on the same curve; the history is replay-critical and public. Impact on past privacy is inferred, not analysed.

## 6. Transport and harvest-now-decrypt-later

- **RLPx (T26).** The ephemeral ECDH key pair of each connection is sent only inside ECIES messages encrypted to the peer's static public key, which is the public enode ID. The session secrets are derived from the ephemeral ECDH output and nonces (`rlpx.go:473-505`). A recording of the handshake plus a CRQC therefore lets an attacker recover the static key (ECDH on secp256k1 is a discrete-log problem), decrypt the auth and ack messages, learn the ephemeral public keys, and solve the discrete log for an ephemeral key to derive the session secrets. Forward secrecy against a *classical* static-key compromise does not survive a discrete-log break, because both layers are the same curve (analysis from the code; not run).
- **What is at stake.** XDC chain data on the wire is public (blocks, transactions, votes). HNDL exposes the pre-inclusion content and timing of transactions and which peers talk, not secrets of the chain. The practical impact is lower than for signatures. Real-time impersonation of a node needs a CRQC at connection time and is a separate, later concern.
- **ML-KEM is not an identity.** FIPS 203 specifies a key-encapsulation mechanism [S1b]. Replacing the RLPx key exchange with ML-KEM protects confidentiality of the session but does not authenticate the peer. Authentication would still rest on the static secp256k1 identity unless a PQ signature identity replaces it. Hybrid key establishment (NIST IR 8547 section 3.2.1) needs the order and combiner fixed at protocol level [S5].
- **Frames (T27)** and keystores (T23) are symmetric (AES). See section 1.

## 7. Domain-separation gaps found in today's formats

These are classical properties, observed in code, that constrain any PQ design.

1. **Timeouts are not chain specific.** `TimeoutForSign{Round, GapNumber}` (`core/types/consensus_v2.go:238-243`) hashes no block hash and no chain ID. A timeout signed on one XDC network for round R and gap G has the same signed bytes on every other network. If a key is a masternode on two networks, a timeout signature from one counts on the other. Effect is one signature toward a TC; not a safety break on its own, but it shows the domain is missing.
2. **Votes and timeouts have no explicit tag.** Their separation is the RLP shape (a 2-element list whose first element is a list versus an integer). That works today; it is not a property a new algorithm should inherit.
3. **M1 and M2 sign the same digest** (T10). Role separation is by assigned address only.
4. **V1 seal digest excludes `penalties`** by design (F-2026-19621 comment, `xdpos.go:207-220`). The binding is by a hook compare, skipped on the bulk-sync light path per the same comment (as written there; not re-verified).
5. **The seal and certificate paths differ on low-S.** Certificate verification rejects high-S signatures (`utils.go:86`, `:197-203`); single vote verification and header seal recovery do not check it in the lines read (`utils.go:55-75`, `xdpos.go:268-295`, `engine.go:318-335`), and the block hash includes the signature (`Header.Hash`, `block.go:137`). Whether geth-XDC (libsecp256k1 recovery) accepts a high-S header seal was not tested, and the vectors contain none. This is a question for #9, not a PQ finding (Q4).

`crates/xone-primitives/src/crypto.rs` carries a `Domain` for every use and a stable tag per domain, so a later algorithm can bind them (FIPS 204 allows a context string of at most 255 bytes [S2]). Today's secp256k1 verifier ignores the tag: the signed bytes are unchanged.

## 8. Downgrade and fallback paths

- **Classical-OR-PQ gives no PQ authentication.** If a verifier accepts either an ECDSA or a PQ signature for the same authority, an attacker needs to break only the weaker one. The skill rule `xone-rs-pq-readiness` is: where protection needs both signatures, require both. NIST IR 8547 section 3.2 describes hybrids as designed to stay secure if at least one component holds, i.e. an AND of components, and notes added complexity and risk [S5].
- **Algorithm choice must not be attacker-chosen.** The algorithm of a signature has to come from the registered key or the protocol version, not from a field the signer controls. The `Verifier` interface takes a tagged `Signature` and the `Secp256k1Verifier` rejects any other id; unassigned and experimental ids (`0xF000-0xFFFF`) are errors in `AlgorithmId::from_id`. No wire or consensus path uses them.
- **Mixed certificates.** A QC made partly of ECDSA votes and partly of PQ votes is only as strong as its ECDSA part. A migration needs a per-epoch rule that retires ECDSA for validators after their PQ keys are registered, and that rule must be part of the QC validity check.
- **Version negotiation.** Peers negotiate `eth/100` and `eth/101` (#32). Any PQ transport must not be removable by a peer that offers only the old version, if the node's policy requires PQ. Policy decision for #20.
- **Fallback owners and recovery paths** (section 5) are the same downgrade seen from the account side.
- **Hybrid cost.** Two signatures per message double storage in QCs and headers; see the arithmetic below.

**Arithmetic on header size (not a measurement).** The Apothem sample has up to 78 signatures in a header's QC (inventory section C). At 65 bytes that is 5,070 bytes. The same count of ML-DSA-65 signatures (3,309 bytes each, FIPS 204 Table 2 [S2]) is 258,102 bytes per header, excluding the other QC fields, keys and any hybrid ECDSA copy. The QC is in every V2 header, so the choice of QC format sets the block-size and history-size budget.

## 9. What Track B must decide (#19, #20)

Track B is research and proposals. Each item needs a written specification, review, audit, a multi-client devnet and governance before any activation.

**#19 Accounts, wallets, validators**
1. Account authority model: new transaction type, delegated verifier, or registry. How the PQ public key is carried (a ML-DSA-65 key is 1,952 bytes [S2]) and whether it is revealed at registration or at first use. A recoverable-signature `Verifier::recover` fits ECDSA only; a PQ scheme needs a registry-based `verify(pubkey, ...)`.
2. Algorithm and parameter set per surface (ML-DSA, SLH-DSA, or a stateful scheme for infrastructure keys under SP 800-208 with its state-reservation burden). Explicitly out of scope here.
3. Address derivation for PQ accounts, with a domain tag (no copying another chain's encoding; `xone-rs-pq-readiness` rule 5).
4. The legacy ECDSA policy (section 5): retire, freeze, time-lock, or leave open. Hybrid rule: both signatures where protection is needed.
5. Validator keys: separate keys per role (seal, vote, timeout, sign), registry (extension of 0x88 or a new contract), rotation, and the QC format and size limit (section 8 arithmetic). Whether a QC stays a list of individual signatures. Chain ID or genesis hash in every domain (section 7).
6. Verification cost for valid and invalid inputs per message size (#18 cost lab), since the txpool and the vote path verify attacker-supplied signatures.
7. Activation: fork block, mixed-version behaviour, rollback, and the multi-client gate in `xone-rs-pq-readiness` ("Gate Q").

**#20 Transport, history, bridges, release**
1. Peer identity and transport: PQ identity signatures versus ML-KEM-only key establishment (which does not authenticate), the discovery and ENR record scheme and size limits, DNS tree signatures (T45).
2. History and checkpoints: how a node authenticates an anchor without trusting exposed historical ECDSA keys (section 4, item 3). Connects to #14 and P10.
3. Bridges, oracles and signature-verifying contracts (T44, T32): enumerate them on chain (the client cannot), their signer sets, and their migration owners.
4. Release and snapshot signing (T42, T43, P08, P10): scheme, key ceremony, custody, rotation, revocation, and downgrade prevention for old artifacts.
5. Custody (T23, T24): HSM, smartcard and remote-signer support for PQ keys; at-rest encryption for node keys.
6. XDCx privacy history (T41).

**Open questions for Track A (to settle before closing #17)**
- Q1. Address strength: how does a 160-bit hash truncation of the public key fare against quantum search? No primary source is cited here; #18 must source it before any statement.
- Q2. Legacy-balance measurement (section 5).
- Q3. Unknowns marked in the inventory: T19 (0x88 access control), T20 (foundation wallet custody), T21 (upgradeable contracts), T24 (hardware or external signer use), T42 (release pipeline), T44 (bridges).
- Q4. High-S acceptance on header seals (section 7, item 5).
- Q5. Non-PQ observation for the security owner: `contracts/utils.go:278-286` builds the randomize key with `math/rand` seeded from `time.Now().UnixNano()` (not a CSPRNG); no emitter exists at the pin, so this concerns replay of V1 history and any other client that still emits it.

## 10. Sources (primary; accessed 2026-11-11)

- [S1] NIST, FIPS 203 final page, https://csrc.nist.gov/pubs/fips/203/final (status, history, planning note 2025-11-17). [S1b] FIPS 203 PDF, https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.203.pdf (abstract). Errata (potential updates): https://csrc.nist.gov/files/pubs/fips/203/final/docs/fips-203-potential-updates.xlsx
- [S2] NIST, FIPS 204 final page, https://csrc.nist.gov/pubs/fips/204/final (planning notes 2026-07-31 and 2025-01-31). PDF, https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.204.pdf (Table 2; context string in Algorithms 2 and 3). Errata (potential updates): https://csrc.nist.gov/files/pubs/fips/204/final/docs/fips-204-potential-updates.xlsx. FAQ: https://csrc.nist.gov/Projects/post-quantum-cryptography/faqs#Rdc7
- [S3] NIST, FIPS 205 final page, https://csrc.nist.gov/pubs/fips/205/final; PDF, https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.205.pdf (abstract).
- [S4] NIST, SP 800-208, https://csrc.nist.gov/pubs/sp/800/208/final
- [S5] NIST, IR 8547 final (October 2026), https://csrc.nist.gov/pubs/ir/8547/final; PDF, https://nvlpubs.nist.gov/nistpubs/ir/2026/NIST.IR.8547.pdf (Tables 1, 2, 4; section 3.2). Draft page: https://csrc.nist.gov/pubs/ir/8547/ipd
- [S6] Roetteler, Naehrig, Svore, Lauter, "Quantum resource estimates for computing elliptic curve discrete logarithms", https://arxiv.org/abs/1706.06752 (v3).
- Code: XDCIndia/go-ethereum at `6fc8e8c2088b7f18264f5663036669d04fa7becf`, as cited per row in the inventory. xOne issues #1, #12, #14, #17, #18, #19, #20, #32, #40, #46 read with `gh issue view -R XDCIndia/xOne` on 2026-11-11.
