# Proposal B2: history and checkpoint authentication for fresh bootstrap and light clients

Issue [#20](https://github.com/XDCIndia/xOneRS/issues/20) (XA-15), track B research. **Research only. No change to the network, no code in shipping crates.** Gate: [governance-activation.md](governance-activation.md).

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`. Rows `T..`, `P..` from [`../inventory.md`](../inventory.md).

## 1. Problem

A fresh node trusts something to find the chain. Today that something is classical:

| Surface | Today | Source |
| --- | --- | --- |
| geth-XDC trusted checkpoints | A list in `params/config.go` (`TrustedSyncCheckpoints`, `:2149-2162`; mainnet 19 entries, Apothem 16); no signature on a snapshot, manifest or checkpoint was found; the only Ed25519 use in the tree is `crypto/signify` | inventory T43; `spec/bootstrap-anchor.md` "What geth-XDC does" |
| geth-XDC QC-verified pivot | The candidate's three children carry QCs verified against the epoch-switch list fetched **from the serving peer**; the code says the peer is the trust root | `spec/bootstrap-anchor.md`, `engine_v2/snap_pivot.go:218-258` |
| xOne anchor | An RLP manifest binding `chain_id`, `genesis_hash`, checkpoint `(number, hash)`, `state_root`, validators and a proof of headers whose QCs verify against that validator list; freshness by `created_at` and `max_age`; **one 32-byte pin must come from outside the manifest** | `spec/bootstrap-anchor.md`, `xone_sync::bootstrap::verify_anchor` |

The anchor proof is made of ECDSA signatures (T09-T15): votes and QCs of the validators. After a quantum computer exists, the keys of validators who have ever signed are recoverable from public data. An attacker can then build a **long-range history**: an alternative chain, or a forged anchor, whose certificates verify against an old validator list. A signature change in the future does not make old signatures unforgeable (issue #20, Why). The spec already says "a stale set is the usual way a long-range history is made to look signed" and bounds it with freshness.

## 2. Assumptions

1. Historical ECDSA signatures stay public and become forgeable after a CRQC; nothing can re-sign history.
2. A new node always needs at least one trust input from outside the chain. The aim is that the input is authenticated by a key that is not forgeable at the time of use, and that old anchors expire.
3. Light clients verify headers and certificates; they cannot execute the chain to check an election (`spec/bootstrap-anchor.md`, "What is still trusted").
4. No PQ scheme or library is chosen (`../implementations.md`).

## 3. Proposal

### 3.1 Compatible part: a signed manifest (no network change)

Anchor and snapshot manifests (P10) gain an optional **signature block** over the whole manifest bytes, from a **manifest key** held by the manifest producer, using a hybrid pair (a classical signature and a PQ signature, both required). The anchor format already reads `version` first and refuses an unknown one (`UnsupportedFormat`), so this is a new `version`, not a change of version 1.

- What it adds: the "one hash from outside" becomes "one public key (or key hash) from outside", which can be published once and rotated, instead of a fresh 32-byte hash per anchor.
- What it does not add: it does not prove the validators were elected. The existing caveat stays (a chain built from its own keys passes every structural check, test `a_manifest_signed_by_its_own_keys_is_not_pinned`).
- It starts without any network decision because it is an operator-side and release-side artifact (see [bridges-release-signing.md](bridges-release-signing.md) for the shared key ceremony).

### 3.2 Anchor freshness stays the main defence

`max_age` and the verifier's own cap (`Context.max_age_cap`) already limit how long an old validator set can be used. For a PQ-aware policy the cap should be the **shorter** of the cap and the time since the last validator key rotation recorded in the registry (see [validators-qc-tc.md](validators-qc-tc.md)), so that an anchor older than a rotation is not accepted. This is a rule of the verifier, not a consensus rule.

### 3.3 Coordinated part: post-quantum co-attestation of checkpoints

Validators (or a registry-named committee) co-sign a **checkpoint statement** at epoch boundaries with their PQ role key: `domain || chain_id || genesis_hash || number || hash || state_root || epoch || validator_set_hash`. The statement and signatures are published outside block data (a sidecar or a service), not in headers.

- A fresh node or light client verifies the co-attestation against the registry set at that epoch; it needs the registry root from the anchor (an element of the anchor `provenance`).
- Hybrid rule: an attestation counts when the PQ signature verifies; during the registration window the classical vote is also required (AND, see the validators proposal).
- Because the statement is not in headers, it needs **no change to block validity** and so no fork. It still needs the registry (a consensus change) to be meaningful, which is why this part is coordinated.
- Size: one signature per validator per attestation. At 108 validators and ML-DSA-65 that is 108 x 3,309 B = 357.37 kB per statement (arithmetic, `../bandwidth-model.md` table 4 last row); if attested once per epoch (900 blocks, 30 minutes) that is 357.37 kB per epoch, not per block.

### 3.4 Light clients

A light client that follows headers needs the validator list and a way to know it is current. Options (research questions):

1. Carry the registry root in the header (changes the header, a consensus change).
2. Verify a chain of epoch-switch headers from a trusted anchor, each certified by the previous list, using the attestation of 3.3 as the PQ link.
3. A sync-committee-style small signing set (a different safety assumption; not proposed).

The geth-XDC light verifier at the pin has empty bodies (inventory T56), so there is no existing light-client verification to migrate.

## 4. Downgrade analysis

| Attack | Defence in this proposal |
| --- | --- |
| Present an old classical-only anchor | Freshness cap and registry rotation (3.2); a node policy that requires the manifest signature (3.1) refuses an unsigned anchor |
| Strip the PQ half of a hybrid manifest signature | Both halves are required (AND); a missing half is an error, not a downgrade |
| Serve a long-range chain with forged old QCs | The attestation (3.3) is signed with keys that are valid now; old QCs alone do not satisfy a policy that requires it |
| Substitute a different validator set in `provenance` | The pin from outside names the key (3.1) or the checkpoint hash; the attestation binds `validator_set_hash` |
| Replay an old valid manifest | `created_at` and `max_age`, and the epoch in the attestation |

## 5. Budgets

- Manifest signature block: one hybrid signature, for ML-DSA-65 3,309 B plus the classical signature, once per manifest. Negligible against a snapshot.
- Attestation: 357.37 kB per epoch at 108 validators (3.3), served on demand.
- Verification: one PQ verify per attester, 94.5 us p50 for ML-DSA-65 in the lab (`../lab-results-2026-11-11.md`), 108 of them about 10.2 ms (`../bandwidth-model.md` table 5, votes column). Single-host, shared-host numbers.

## 6. Mixed versions and rollback

- 3.1 changes no node that does not read it: an old verifier refuses `version` 2 manifests by the existing rule, so producers publish both versions during rollout.
- 3.3 is additive and off-chain; nodes that ignore attestations behave as today.
- Rollback: stop publishing version 2 manifests and attestations. Nothing on chain depends on them until the registry exists.

## 7. Devnet vector plan (no activation)

| Vector | Expect |
| --- | --- |
| V-D1 version 2 manifest with a valid hybrid signature | verifies; the structural checks of version 1 still pass |
| V-D2 same manifest with the PQ half removed or corrupted, and with the classical half removed or corrupted | refused in each case |
| V-D3 manifest signed by an unpinned key | refused (`NotPinned`-style) |
| V-D4 old manifest after a registry rotation | refused by the freshness rule |
| V-D5 attestation with q-1 valid signatures | below threshold |
| V-D6 attestation for another chain id or genesis | refused |
| V-D7 two clients verify the same anchors and attestations | identical verdicts |

## 8. Open questions

1. Who holds the manifest key and how it is rotated and revoked (shared with release signing).
2. Registry root in the anchor `provenance`, or in the header.
3. Attestation cadence (per epoch is assumed above).
4. Whether the geth-XDC `TrustedSyncCheckpoints` list needs signatures too, or only xOne's anchor.
