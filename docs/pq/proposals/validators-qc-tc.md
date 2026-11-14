# Proposal A2: PQ validator keys, votes, QC and TC

Issue [#19](https://github.com/XDCIndia/xOneRS/issues/19) (XA-14), track B research. **Research only. No activation, no change to the network.** See [governance-activation.md](governance-activation.md) for the gate.

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`. Rows `T..`, `P..` from [`../inventory.md`](../inventory.md); numbers from [`../bandwidth-model.md`](../bandwidth-model.md) (arithmetic on the lab sizes, not a network measurement).

## 1. What exists today

| Surface | Today | Row |
| --- | --- | --- |
| V1 seal (M1) and double validation (M2) | secp256k1 over the same digest; M1 and M2 differ only by assigned address | T09, T10 |
| V2 proposer signature | secp256k1 over the header hash with full `extraData` | T11 |
| V2 vote | secp256k1 over `keccak256(rlp(VoteForSign{ProposedBlockInfo, GapNumber}))`, 65 B | T12 |
| V2 timeout | secp256k1 over `keccak256(rlp(TimeoutForSign{Round, GapNumber}))` | T13 |
| QC and TC | a list of individual 65-byte signatures, no aggregation; threshold `count >= masternodes * 0.667`; peer intake cap 150 signatures | T14, T15 |
| Sign transaction to 0x89 | an ordinary ECDSA transaction | T17 |
| Key custody | one keystore-unlocked key signs T09-T13 and T17 | T22 |
| Masternode registry | contract 0x88 by `msg.sender` | T19 |

Observed domain gaps that a new format must not inherit (threat model section 7): timeouts carry no block hash and no chain id, so the same bytes are valid on every XDC network for the same round and gap; votes and timeouts have no explicit tag (separation is the RLP shape); M1 and M2 sign the same digest.

## 2. Assumptions

1. No aggregation exists for ML-DSA or SLH-DSA. QC and TC stay lists of individual signatures (`bandwidth-model.md` A4).
2. 108 masternodes at most; 73 signatures is the quorum minimum at threshold 0.667; the Apothem sample reaches 78 and the median is 21 (`bandwidth-model.md` A1, A3; inventory section C).
3. Safety rests on at least two thirds of validators holding honest keys; a PQ change must not lower that.
4. A stateful scheme (SP 800-208) is considered only for infrastructure keys, never silently: it needs a durable one-time-state reservation that a datadir rollback never restores (`../../adr/ADR-010-signer-safety.md`; `xone-rs-pq-readiness` rule 6).

## 3. Proposal

### 3.1 Role keys and a registry

- **Separate keys per role**: seal (V1 and V2 proposer), vote, timeout, sign-tx. Today one key does all (T22). A leak of a vote key then does not give a seal key. Cost: four registrations per validator.
- **Registry**: an extension of 0x88 (T19) or a new system contract, mapping the validator address to `(role, alg_id, public_key, valid_from_epoch, valid_to_epoch)`. A new key becomes valid at an epoch boundary only, so every node sees the same set for a round (epoch-switch semantics, `spec/consensus/v2-epoch-switch-and-gap-snapshot.md`).
- **Registration is authorised by both keys**: the existing ECDSA key and a proof of possession by the new PQ key (AND).
- **Rotation and revocation**: a revocation takes effect at the next epoch boundary; an emergency revocation path (owner of the candidate in 0x88) is an open question.
- **Remote signer**: the fencing-token interface (`docs/ops/keys.md`, P05, ADR-010) is algorithm-agnostic; a PQ key behind it needs the same persist-before-send rule for the highest voted round.

### 3.2 Message domains

Every signed message gets an explicit domain: `tag || chain_id || genesis_hash || payload`. The 255-byte context of FIPS 204 can carry the tag [S2]. `crates/xone-primitives/src/crypto.rs` already has a `Domain` per use (`SealV1`, `Validator`, `VoteV2`, `TimeoutV2`, `SyncInfoV2`) and ignores it for secp256k1, so the signed bytes of today are unchanged. For timeouts the chain id and genesis hash close the cross-network gap of threat model section 7, item 1.

### 3.3 Hybrid rule and QC format

| Phase | A vote counts toward a QC if | QC contents |
| --- | --- | --- |
| Before activation | ECDSA verifies (today) | list of ECDSA signatures |
| Registration window (`hybrid`) | ECDSA **and** PQ verify, for validators with a registered PQ key; ECDSA only for the others | both signatures per counted vote; size adds (`bandwidth-model.md` A11) |
| After a per-validator cut-over epoch | PQ verifies; an ECDSA-only vote from that validator is invalid | PQ signatures only |

AND, not OR: a QC made partly of ECDSA votes is only as strong as its ECDSA part (threat model section 8). The rule that retires ECDSA for a validator after its PQ key is registered must be part of QC validity, not a node policy.

### 3.4 QC size: the real cost

A header carries the parent QC (T14). With ML-DSA-65 (3,309 B per signature) and no aggregation:

| Signatures in the QC | ECDSA per header | ML-DSA-65 per header | ML-DSA-65 growth per epoch (900 blocks) |
| --- | --- | --- | --- |
| 8 (net2040 median) | 520 B | 26.47 kB | 23.82 MB |
| 21 (Apothem median) | 1.36 kB | 69.49 kB | 62.54 MB |
| 73 (quorum minimum at 108) | 4.75 kB | 241.56 kB | (table 4) |
| 78 (Apothem maximum in the sample) | 5.07 kB | 258.10 kB | (table 4) |
| 108 | 7.02 kB | 357.37 kB | (table 4) |

Votes at 108 voters per 2 s round: ECDSA 13.93 kB per round; ML-DSA-65 364.28 kB per round, 182.14 kB/s (`bandwidth-model.md` table 3). Verification CPU for the QC of 78 and 108 votes per round at the lab p50: 7.37 ms per block and 10.20 ms per round for ML-DSA-65 against 11.96 ms and 16.56 ms for ECDSA `recover` (table 5). Verification is not the problem; bytes are. SLH-DSA is larger still (7,856 B at the smallest set); with SLH-DSA-128s the 78-signature QC takes 139.81 ms per block and 108 votes take 193.59 ms per round to verify (table 5, single core), so it is not a candidate for the vote path.

Ways to bound the size, each a research question and none a proposal:

1. **Sidecar QC**: the header carries a hash of the QC and the signatures travel and are stored separately. Changes the header format and the block hash input; needs a consensus change of its own.
2. **Per-epoch committee for certificates**: a subset signs. This changes the safety argument (assumption 3) and must not be adopted without a new analysis.
3. **Dropping individual signatures after finality** (prune old QCs): a history-and-light-client question, see [history-checkpoints.md](history-checkpoints.md).
4. **Aggregation research** for a PQ scheme: none is standardised; no assumption is made.

### 3.5 Timeouts, TC, SyncInfo

The TC is a list of timeout signatures (T15) and SyncInfo carries a QC and a TC (T16). They follow the QC rules above, with the chain id and genesis hash in the timeout domain. The intake cap of 150 signatures per peer message (T14) is in bytes about 150 x 3,309 B = 496 kB for ML-DSA-65 (arithmetic), so the cap needs a byte limit as well as a count.

## 4. Downgrade protection

- The algorithm comes from the registry entry for the validator and role, never from the message.
- A QC with a mix of algorithms is judged per validator by that validator's mode and cut-over epoch.
- A peer cannot remove the PQ signature and ask a node to accept the ECDSA one once the validator is in `pq-only` mode.
- Version negotiation (`eth/100`, `eth/101`, `spec/net/eth-versions.md`) is the wire side of the same rule; see [transport-identity.md](transport-identity.md).

## 5. Mixed versions and rollback

- Activation is a fork block per network, `Unscheduled` until then. Nodes without the rules reject the first QC with a PQ signature and fork off; one client cannot activate it.
- During the registration window old and new nodes must agree on every QC; the AND rule makes a hybrid QC valid for an old node only if the old node ignores the extra signature. That is a header-format compatibility question for the activation spec, not solved here.
- Rollback past the activation is a second governed fork. The safety record (highest voted round, `ADR-010`) must never be restored from a backup that is behind it; the same applies to any PQ key state.

## 6. Devnet vector plan (no activation)

| Vector | Expect |
| --- | --- |
| V-B1 registration of a seal, vote and timeout key with both proofs | accepted at the next epoch boundary, not before |
| V-B2 vote with ECDSA only after cut-over | not counted |
| V-B3 QC with q-1 valid PQ votes and one invalid | below threshold, rejected |
| V-B4 QC mixing an ECDSA-only vote for a `pq-only` validator | that vote not counted |
| V-B5 timeout signed for chain A replayed on chain B | rejected (domain with chain id) |
| V-B6 vote digest replayed as a timeout, and as a transaction | rejected (tags) |
| V-B7 byte-limit test: QC larger than the byte cap | rejected before verification |
| V-B8 epoch switch with a key rotation | every node computes the same signer set |
| V-B9 two clients on the same chain through a rotation and a restart with a stale datadir | no double sign, identical roots |

## 7. Open questions

1. QC format: keep a list (size above), or a sidecar.
2. Whether a stateful scheme is allowed for any validator role.
3. Emergency revocation authority.
4. Registry home: 0x88 extension or a new contract (T19 access control is unknown).
5. Whether to record the chain id and genesis hash in the V1 seal and V2 proposer domains too (T09-T11).
