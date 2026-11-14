# Proposal B1: P2P identity and transport (RLPx, discovery, ENR)

Issue [#20](https://github.com/XDCIndia/xOneRS/issues/20) (XA-15), track B research. **Research only. No change to the network, no code in shipping crates.** Gate: [governance-activation.md](governance-activation.md).

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`. Rows `T..`, `P..` from [`../inventory.md`](../inventory.md).

## 1. What exists today

| Surface | Today | Row |
| --- | --- | --- |
| RLPx handshake | ECIES on secp256k1 (ECIES_AES128_SHA256) encrypts auth and ack to the peer's static public key; static-static ECDH gives a token; ephemeral ECDH gives the session secrets | T26 |
| RLPx frames | AES-256-CTR with a Keccak-based MAC; symmetric | T27 |
| Node identity | secp256k1 key; node id `keccak256(x || y)`; ENR scheme "v4" signs the record with `crypto.Sign` | T28 |
| discv4 | each packet has a 32-byte hash and a 65-byte secp256k1 signature; XDC sends ping as packet type 5 | T29 |
| discv5 | inherited; ECDH on secp256k1 and an ID signature | T30 |
| DNS discovery lists | secp256k1 signature over an EIP-1459 root | T45 |
| Node key file | `<datadir>/nodekey`, hex, mode 0600, not encrypted | T46 |
| xOne | `xone-net`: discv4 with the XDC ping, RLPx over `reth-ecies` (unmodified, ADR-002 decision 3), own eth/100, eth/101 | P07 |

## 2. The two threats, and why they differ

- **Harvest now, decrypt later (HNDL).** A recorded RLPx handshake plus a future quantum computer lets an attacker recover the static key (ECDH on secp256k1 is a discrete-log problem), decrypt auth and ack, and then recover the session secrets (threat model section 6). On XDC the content on the wire is public chain data (blocks, transactions, votes); HNDL exposes timing and pre-inclusion transaction content and which peers talk. The practical impact is lower than for signatures.
- **Impersonation.** Forging a node identity needs the quantum computer at connection time. It is a later, separate concern.

## 3. Assumptions

1. ML-KEM (FIPS 203) is a key-encapsulation mechanism. It protects confidentiality of a session. **It does not authenticate the peer.** Authentication stays on the secp256k1 identity until a PQ signature identity replaces it (threat model section 6).
2. NIST recommends ML-KEM-768 as the default set (IR 8547 Table 5, [S5]). Sizes: encapsulation key 1,184 B, ciphertext 1,088 B, shared secret 32 B (`../standards.md` section 3).
3. Hybrid means both components are needed (AND). A combiner that is secure if either component holds is the NIST IR 8547 section 3.2 reading ([S5]); the combiner and its order must be fixed in the protocol.
4. Wire-only: no consensus rule changes. But a peer that does not speak the new version must still be handled by an explicit policy.
5. No library is chosen; libraries are lab input until reviewed (`../implementations.md`).

## 4. Proposal

### 4.1 Hybrid key establishment in a new RLPx handshake version

- The initiator adds an ML-KEM-768 encapsulation key to the auth message; the responder returns the ciphertext in the ack. Both messages stay encrypted to the static secp256k1 key as today, so an eavesdropper who breaks only ECDH does not see the KEM material, and one who breaks only the KEM still faces ECDH.
- Session secrets come from one KDF over: the ECDH output, the ML-KEM shared secret, both nonces, and a **transcript hash** of everything exchanged (both node ids, both Hello capability lists, protocol versions, both ephemeral keys, the KEM key and ciphertext). Binding the transcript is what makes a stripped or substituted component change the keys, so a mismatch fails the first MAC instead of silently downgrading.
- The responder proves it derived the same secret by the first frame MAC, as today.

### 4.2 Downgrade resistance

- The set of versions each side offers is inside the authenticated transcript (hash input above). A middle party that removes the new version from one Hello makes the two transcripts differ.
- A node policy `require-pq-transport` (an operator setting in a future release, not proposed as a consensus rule) refuses sessions that do not complete the hybrid handshake. Without it the node accepts old peers (needed during rollout).
- Version negotiation today is `eth/100` and `eth/101` (`spec/net/eth-versions.md`). The hybrid handshake is a transport version, separate from the eth versions, so a peer that offers only an old transport is refused by policy and not by the eth layer.

### 4.3 Identity

Identity stays secp256k1 in this proposal. A PQ identity (a node id derived from a PQ key, a PQ ENR scheme, signed discv4 packets) is blocked by size limits observed at the pin:

| Limit | Value | Source at the pin | Effect |
| --- | --- | --- | --- |
| ENR record | 300 bytes maximum | `p2p/enr/enr.go:46` (`SizeLimit = 300`) | an ML-DSA-65 signature (3,309 B) or public key (1,952 B) does not fit; a PQ ENR scheme needs a different carrier or a hash of the key |
| discv4 packet | 1,280 bytes maximum | `p2p/discover/v4_udp.go:61-64` (`maxPacketSize = 1280`) | a PQ signature does not fit one packet |
| Hello (protocol handshake) message | 2 KiB | `p2p/transport.go:153-160` (`readProtocolHandshake` refuses `msg.Size > baseProtocolMaxMsgSize`), `p2p/peer.go:44` (`2 * 1024`) | a Hello that carries a PQ public key (ML-DSA-65: 1,952 B) or signature is at or over the cap; the KEM material goes in the RLPx auth and ack, not in Hello |
| Protocol message | 10 MiB | `eth/protocols/eth/protocol.go:91-92` (`maxMessageSize`) | not a constraint |

Options for identity, each a research question: (a) keep secp256k1 for identity and add the hybrid key establishment only (this proposal); (b) a PQ identity proven inside the RLPx handshake (the signature travels in the encrypted ack; the size limit of the auth and ack messages was not checked here and a new cap must be stated); (c) keep discovery classical and authenticate the peer after the handshake. DNS tree signatures (T45) and discv4 stay classical in this proposal.

### 4.4 Budgets

- Added handshake bytes (arithmetic): 1,184 B in auth and 1,088 B in ack for ML-KEM-768, plus any PQ identity signature if option (b). Per connection, once.
- Added handshake CPU: not measured. The cost lab has no ML-KEM row (`../lab-results-2026-11-11.md` section 8; `docs/pq/cost-lab-plan.md` (PR 281) Q6 plans it).
- Frames: unchanged, symmetric (T27). Grover-type margins for AES-256 are outside the analysis (threat model section 1).
- Flood: a handshake from an unauthenticated peer makes the responder do one encapsulation. A per-source rate limit and the existing peer caps apply; the cost needs Q6.

## 5. Mixed versions and rollback

- A node with the new transport and policy off talks to old and new peers; with the policy on it refuses old peers. During rollout, mixed networks are the default state.
- No consensus effect: blocks, votes and transactions are unchanged, so a transport bug cannot change a state root. It can partition the peer graph (availability).
- Rollback is a release rollback: the old transport still works with every peer.
- The fallback of ADR-002 (vendor `reth-ecies` into `xone-net`) matters here: the hybrid handshake cannot be built on an unmodified `reth-ecies`; a vendored copy or an own implementation of the handshake is needed. That is an ADR-002 re-open question for the owner.

## 6. Devnet vector plan (no activation)

| Vector | Expect |
| --- | --- |
| V-C1 hybrid handshake between two xOne nodes, fixed ephemeral keys and KEM randomness | both derive the same session keys; fixed bytes for auth, ack and first frames |
| V-C2 ciphertext or KEM key bit-flipped | first MAC fails; connection closed |
| V-C3 Hello with the new version removed by a middle party | transcripts differ; connection fails |
| V-C4 old peer connects to a `require-pq-transport` node | refused with a typed reason |
| V-C5 old peer connects to a node with the policy off | classical session as today |
| V-C6 replay of a recorded auth message | no session (fresh nonces) |
| V-C7 two independent clients over the same vectors | identical derived keys |

## 7. Open questions

1. Which hybrid combiner and KDF; NIST guidance on combiners must be read at the time (not cited here beyond IR 8547 section 3.2).
2. Identity option (a), (b) or (c).
3. Whether the node key file needs encryption at rest for PQ keys (T46 stores it unencrypted).
4. Whether to build on `reth-ecies` (not possible without change) or vendor it (ADR-002 decision 5).
