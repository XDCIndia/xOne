---
name: xone-rs-pq-readiness
description: Post-quantum migration readiness for the xOne rebuild (track Q). Inventory of every authority and cryptographic use, a threat model, pinned standards, and a cost lab, as research and compatible interface work only. No activation without an approved protocol spec and a multi-client devnet. Use when the task says post-quantum, PQ, ML-DSA, ML-KEM, SLH-DSA, FIPS 203, 204 or 205, quantum, key registry, hybrid signature, or crypto inventory.
---

# xone-rs-pq-readiness

The goal is **PQ migration readiness** under stated security assumptions. It is not "quantum proof": no language and no single library can promise resistance to all future attacks. Track Q runs in parallel with gates A to E. Its activation path depends on an approved protocol, ecosystem readiness, audits and a devnet gate, not on A to E alone.

## Operating rules

1. **No activation.** A single client cannot activate incompatible rules. Nothing from this track enters a production build as an active rule. Isolated research code lives outside shipping builds.
2. **Two tracks.** Compatible engineering (inventory, versioned internal interfaces, benchmark harnesses) may proceed now. Protocol work needs a written specification, review, an audit, a multi-client devnet, wallet and signer support, governance and staged activation.
3. The wire and consensus freeze of the compatible rebuild stays. A PQ change is a separate, versioned, coordinated proposal.
4. Claims about security, speed or size cite a pinned source or a measurement with its inputs. Arithmetic is labelled as arithmetic.
5. Do not copy another chain's format as a convention (a different commitment hash changes state commitments). Borrow ideas, not encodings.

## Deliverables

1. **Inventory.** Every authority and cryptographic use: accounts (ECDSA/ecrecover, exposed public keys, envelopes, recovery and upgrade authorities); validators, QC and timeout messages (key registries, domains, size limits); P2P identity and transport (RLPx secp256k1 ECIES/ECDH, discovery); history, checkpoints and light-client trust; bridges, governance, HSM, release signing and custody; hashes and proof systems (Keccak, address derivation, precompiles, pairings). Mark each as observed, inferred or unknown.
2. **Threat model.** Per surface: the attacker capability, the weakest authority path (a PQ verifier does not protect an account if a legacy owner or fallback can still move funds), harvest-now-decrypt-later versus forgery, downgrade and fallback. Hybrid rule: where protection needs both signatures, require both; an OR fallback leaves the classical forgery route open.
3. **Standards pins.** FIPS 203 (ML-KEM, key establishment only, not identity), FIPS 204 (ML-DSA), FIPS 205 (SLH-DSA), SP 800-208 for stateful hash signatures. Record exact versions, parameter sets and applicable errata, with the access date. Re-check at each use.
4. **Cost lab.** Measured, reproducible: sizes, sign and verify cost for valid and invalid inputs, per message size, pinned implementation and CPU. Bandwidth model: for example, ML-DSA-65 has a 1,952-byte public key and a 3,309-byte signature (FIPS 204 Table 2), so one signature per transaction at 3,000 TPS is 9,927,000 bytes/s of signatures alone, about 79.42 Mbit/s, before keys, payloads, votes, fanout and retransmission. That is arithmetic, not a measured network result.
5. **Implementation assurance.** For each candidate library: audit status (for example, the inspected `ml-dsa 0.1.2` documentation says it has had no independent audit; liboqs describes a prototyping role), platform support, constant-time and side-channel review, entropy and zeroization, conformance vectors, dependency provenance. A pure-Rust implementation or a FIPS algorithm name is not module validation.
6. **Stateful schemes** (if studied): durable one-time-state reservation across restore, failover and retries; see `xone-rs-durability-signer-gate`. A datadir rollback must never restore signer indices.
7. **Research record.** What was reviewed, what was excluded, and why, per subsystem.

## Gate Q (activation) conditions

All must hold before any activation proposal is accepted: an approved protocol specification per surface; independent audit scope and result; vectors that pass in at least two independent clients; a multi-client devnet run including malformed-signature floods, key rotation, mixed-version behavior and remote-signer recovery; wallet and signer readiness; a governance decision. Until then the output is documents, benchmarks and interfaces.

## Done

The inventory, threat model, standards pin table and cost-lab report are on the work item with commit, host and CPU for each measurement. The report states "no activation, no security claim implied".
