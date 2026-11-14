# Proposal B3: bridges, governance and upgrade keys, custody, and release and snapshot signing

Issue [#20](https://github.com/XDCIndia/xOneRS/issues/20) (XA-15), track B research. **Research only. No change to the network, no code in shipping crates.** The release-signing part has a **compatible** step that needs no network decision; it is flagged below. Gate for everything else: [governance-activation.md](governance-activation.md).

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`. Rows `T..`, `P..` from [`../inventory.md`](../inventory.md).

## 1. What exists today

| Surface | Today | Row or file |
| --- | --- | --- |
| Bridges and cross-chain | None in the client. Any bridge or oracle contract on XDC and its signer set (typically a multisig or threshold ECDSA over secp256k1, or a validator set) is outside what the client can list | T44 |
| Contracts that verify ECDSA | Precompile 0x01 and signed-message patterns | T32, T47 |
| Governance and upgrade authority | No on-chain upgrade authority: fork blocks are constants in the client and a rule change needs a client release and operator adoption. The masternode voting contract 0x88 authorises by `msg.sender`; access control and upgradeability of system contracts are unknown | T19, T21 |
| Foundation and team wallets | Addresses only; custody scheme not visible | T20 |
| Validator key custody | Keystore-unlocked secp256k1 key in memory; v3 keystore at rest (scrypt or pbkdf2, AES-128-CTR, Keccak MAC); hardware and external signers all sign secp256k1 | T22, T23, T24 |
| geth-XDC release signing | GPG (PPA) and signify (Ed25519) tooling exists in the build tool; whether the XDC pipeline runs it, with which keys and who holds them, is unknown | T42 |
| xOne release signing | `SHA256SUMS` signed with a detached GPG signature (`SHA256SUMS.sig`); the tarballs, SBOM (CycloneDX 1.5) and in-toto provenance are listed in `SHA256SUMS`; operators run `gpg --verify` then `sha256sum -c`. SBOM and provenance have no signature of their own beyond that. The release key, image signing and the public installer are not decided | `docs/ops/release.md` ("Not covered here"), `ops/install.md` steps 2 to 3, P08, P09 |
| Snapshot and checkpoint trust | No signature on a snapshot, manifest or checkpoint | T43, `spec/bootstrap-anchor.md` |

## 2. Assumptions

1. A chain upgrade cannot secure a third-party bridge alone (issue #20, Why): a bridge secured by ECDSA multisig stays forgeable after any chain rule change unless its owners migrate.
2. A release signature is verified offline by an operator; it can use a scheme the chain does not support. So release signing can move before, and independently of, any consensus change.
3. A release signature is replayable by nature: a signature over an old artifact stays valid forever (threat model section 3 row for release signatures). Rotation and revocation need an explicit design.
4. No PQ scheme or library is chosen. All candidate libraries are lab input until reviewed; none has a stated audit (`../implementations.md`).

## 3. Proposal

### 3.1 Release and snapshot signing (compatible part, can start now)

**Dual signature on the checksum file.** Keep `SHA256SUMS.sig` (GPG) exactly as it is. Add a second detached signature over the same `SHA256SUMS` file from a post-quantum key (working name `SHA256SUMS.pq.sig`), listed in a manifest of signatures so the format is explicit. The hybrid rule is AND: a verifier that requires both refuses a release with either missing or invalid.

| Choice | Candidate | Reason | Cost |
| --- | --- | --- | --- |
| Scheme | SLH-DSA (FIPS 205), stateless hash-based | public key 32 B at the 128-bit sets, conservative assumption (hash functions), no state to protect; signing speed is irrelevant for a handful of release files | signature 7,856 B at the smallest set (`../standards.md` section 3); sign takes about 2.1 s for SLH-DSA-128s in the lab, verification 1.8 ms (`../lab-results-2026-11-11.md` note 6) |
| Alternative | ML-DSA-65 (FIPS 204) | signature 3,309 B, public key 1,952 B, fast | lattice assumptions; larger key to publish |
| Not for releases | Stateful LMS or XMSS (SP 800-208) | a signing key that must never reuse state is a poor fit for a release pipeline that may restore or rebuild hosts (`xone-rs-pq-readiness` rule 6) | state reservation burden |

The choice is the owner's. The libraries behind either option are lab input; a release signer would first need a reviewed implementation and the NIST vectors run against it (`docs/pq/cost-lab-plan.md` (PR 281) Q9). Until then the PQ signature is an additional, non-gating artifact.

**Key ceremony and custody (to write, not done here).** Offline key generation; published key id and fingerprint in the release notes, as the GPG key id is today (`docs/ops/install.md` step 2); a threshold or two-person rule for the signing host; the signing key never on a build host or a validator host; stored encrypted (the keystore pattern, T23) and backed up offline.

**Rotation and revocation.** A signed `keys.json` listing the current and previous key ids with validity windows, itself signed by the previous key and by an offline root; a revocation entry names a key and a date. Old artifacts stay verifiable against the key that was current at their date (the file says which). Without a timestamp service, "signed before revocation" cannot be proved from the signature alone; a transparency log or a dated, countersigned release list is an option for the owner.

**Downgrade.** The installer and the operator guide state that both signatures are required once the PQ key is published. A verifier that accepts either gives no PQ protection (AND rule, threat model section 8). The policy "both required" is a documented operator rule, enforced by the install script when it exists (`one.sh` is undecided, `docs/ops/release.md`).

**Provenance and SBOM.** `xone-<v>.provenance.json` (in-toto Statement v1, SLSA provenance v1 shape) and the SBOM are covered by `SHA256SUMS` and so by both signatures; no extra step. Image signing (not done) should use the same dual rule when it is added.

**Snapshots and anchors.** The manifest signature block of [history-checkpoints.md](history-checkpoints.md) section 3.1 reuses this key ceremony and rotation file.

### 3.2 Bridges and signature-verifying contracts (research, outside the client)

The client cannot enumerate them. The proposal is a **census** and an **owner outreach**, not code:

1. List contracts on XDC that call precompile 0x01 or verify signed messages, from an archive node (method: scan code for the `ecrecover` call pattern and for known multisig and bridge ABIs; the allowed archive access of the spike lane (5 requests per second) is not enough for a full scan, so this needs a different data source, see `spikes/wide-balance/REPORT.md` "Legacy adjudication").
2. For each, record the signer set, threshold and owner.
3. Publish the list with a deadline for owners to move to a PQ-capable verifier once one exists (accounts proposal, section 6: a verification precompile).
4. Bridge operators decide their own migration; the chain offers the precompile and the registry, nothing more.

### 3.3 Governance, upgrade and custody (research)

- **System contracts (T19, T21).** Settle by reading the deployed code and proxy slots at a pinned block whether 0x88, 0x89, 0x90 are upgradeable and by whom. If an ECDSA key can upgrade them, that key is a PQ migration item of the highest priority and belongs in the registry design of the validators proposal.
- **Foundation and team wallets (T20).** Custody is not visible. The owner records, outside the repository, whether they are multisigs and their signer sets.
- **Fork authority (T21).** Fork blocks are constants in releases; the authority is the client maintainers and the operators who adopt. A PQ activation is therefore a release-and-adoption event ([governance-activation.md](governance-activation.md)).
- **Custody (T22 to T24).** A validator key behind a remote signer with a fencing token (`docs/ops/keys.md`, ADR-010) is algorithm-agnostic. Hardware wallets and smartcards that cannot sign a PQ scheme cannot hold a PQ validator key; their roadmap is outside the repository. At-rest encryption for a PQ key reuses the v3 keystore pattern; the node key file (T46) is unencrypted today.

## 4. Downgrade analysis

| Attack | Defence |
| --- | --- |
| Strip the PQ signature from a release | Both signatures required by operator policy and the installer |
| Replace the PQ public key | The key id and fingerprint are in the release notes and in the signed `keys.json`; the operator imports from the owner, as for GPG |
| Present an old signed artifact as current | Version and date in the signed file list; `keys.json` validity windows |
| Compromise of the GPG key only, or the PQ key only | AND: one stolen key is not enough to publish a release that passes a policy requiring both |
| Bridge with ECDSA multisig after a chain upgrade | Not solvable by the chain; census and owner migration (3.2) |

## 5. Budgets

- Release: one extra 7,856 B file (SLH-DSA-128s) or 3,309 B (ML-DSA-65) per release, plus the public key in the notes (32 B or 1,952 B). Verification: milliseconds.
- No network budget: nothing in 3.1 travels on the P2P network.

## 6. Mixed versions and rollback

- Operators who do not verify the PQ signature are as safe as today (GPG still verifies).
- Rolling back is deleting `SHA256SUMS.pq.sig` from future releases; old releases keep both files.
- Nothing here changes consensus or wire bytes.

## 7. Plan and vectors (no activation)

| Step | Output | Network change |
| --- | --- | --- |
| 1 | Written key ceremony and rotation file format | none |
| 2 | A test release signed with a lab key on a private host, verified by two independent tools | none |
| 3 | Install-script check "both required" | none |
| 4 | Contract census report (3.2) | none |
| 5 | System-contract upgradeability report (3.3) | none |

Test vectors for step 2: a valid dual signature; each half missing; each half over a modified file; a signature by an unlisted key; a signature by a revoked key dated after the revocation; an old release against the current `keys.json`.

## 8. Open questions

1. Scheme: SLH-DSA or ML-DSA for releases.
2. Who holds the offline root and the signing key; the two-person rule.
3. Whether to add a timestamp or transparency log for revocation proofs.
4. Owner of the contract census, and the data source for the scan.
5. Whether to publish the PQ signature as non-gating until a reviewed implementation and NIST vectors exist (proposed) or gate on it from the first release.
