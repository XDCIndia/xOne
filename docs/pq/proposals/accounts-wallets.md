# Proposal A1: PQ account authentication, wallets and legacy balances

Issue [#19](https://github.com/XDCIndia/xOneRS/issues/19) (XA-14), track B research. **This is research. It does not change the network and it activates nothing.** No production activation without network governance, independent audits and a multi-client devnet (see [governance-activation.md](governance-activation.md)).

Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`. Row IDs `T..` and `P..` are in [`../inventory.md`](../inventory.md); `S..` sources in [`../threat-model.md`](../threat-model.md) section 10.

## 1. Problem

- Every externally owned account that has sent a transaction has an exposed secp256k1 public key (T01: sender recovery by `Ecrecover`; low-S, `v` in {0,1}). Its authority is the ECDSA key alone. EIP-7702 delegation (T07, T08) installs code at an EOA but the authority is still recovered with `Ecrecover`; sender recovery of ordinary transactions is unchanged (threat model section 5).
- Contracts that verify ECDSA with precompile 0x01 (T32) or signed messages (T47) keep trusting it after any chain-level change. Their state is outside client control.
- NIST IR 8547 (final, October 2026) lists ECDSA at 112 and 128 bits as "Deprecated after 2030, Disallowed after 2035" (threat model section 2, [S5]). A migration has to finish while the classical keys are still trustworthy: authorization of the move must be given with a key that cannot yet be forged.

## 2. Assumptions

1. Only NIST-standardised schemes are in scope for the options: ML-DSA (FIPS 204) for ordinary accounts; SLH-DSA (FIPS 205) as a conservative alternative whose signatures are 7,856 B or more ([`../standards.md`](../standards.md) section 3). No scheme or parameter set is chosen here.
2. No aggregation: every transaction carries its own signature (`../bandwidth-model.md` A4).
3. The algorithm of a signature comes from the registered key or the transaction type, never from a field the signer controls (threat model section 8).
4. Libraries are lab input until reviewed (`../implementations.md`); nothing here assumes a particular crate.
5. The legacy ECDSA path cannot be switched off per address today (`recoverPlain`, `core/types/transaction_signing.go:490-515`; threat model section 5). Any proposal that protects balances must add that capability.

## 3. Options for the account model

| Option | How a PQ-authenticated account works | Gains | Costs and risks |
| --- | --- | --- | --- |
| A. New transaction type | A type byte (not 0x00-0x04, and not 0x05, which is the EIP-7702 authorization magic of T08) carries `alg_id`, signature and, at first use, the public key. The sender is the address derived from the key | Clean separation; a PQ account has no ECDSA fallback by construction | New address derivation and a new account class; every wallet, explorer and contract that assumes `ecrecover` must change; address derivation needs a domain tag (threat model section 9, item 3) |
| B. Key registry (system contract or state field) | An existing address registers a PQ key. From the registration block, a transaction from that address must carry a valid PQ signature in addition to, or instead of, the ECDSA signature, according to the account's mode | Addresses and balances do not move; contracts that know the address keep working; fits the 0x88-style system contract pattern (T19) | Registration is the weak moment: it must be authorised by ECDSA while ECDSA is still trusted, plus a PQ proof of possession (both required). A rule per account mode must be part of consensus |
| C. Delegated verifier (code at the EOA, EIP-7702 style) | The EOA delegates to code that verifies a PQ signature carried in the transaction data | No new transaction type | The ECDSA key still authorises the EOA itself (T08, T01), so it protects nothing unless the legacy path is also closed; gas for verification must be a precompile or a very expensive contract |

Direction for review: **B with a type-A style envelope for the PQ signature**: registration with both signatures, then a per-account mode that can be moved only one way.

| Mode | Valid transaction from the address | Moves to |
| --- | --- | --- |
| `classical` (today) | ECDSA only | `hybrid` |
| `hybrid` | ECDSA **and** PQ, both must verify (AND) | `pq-only` |
| `pq-only` | PQ only; any ECDSA-only transaction from this address is invalid | none (no way back) |

AND, never OR: a verifier that accepts either signature leaves the weaker route open (threat model section 8). `pq-only` is what actually closes the classical forgery route for that address. A transaction type that is not in the account's mode is invalid at admission and at execution.

## 4. Legacy balances: the decision that is not technical

The client cannot say how much value sits in exposed-key accounts (threat model section 5, Q2). Policy options, for the owner and the network to choose; this proposal does not choose:

| Policy | Effect | Main risk |
| --- | --- | --- |
| P0. No forced move | Holders opt in to `hybrid` and `pq-only` | Unmoved balances stay forgeable after a CRQC |
| P1. Deadline, then time-locked claim | After block D, an `classical` account can move funds only through a delayed, cancellable path that a PQ-registered owner can veto | Needs a delay window and a cancel rule; contracts holding funds cannot veto |
| P2. Deadline, then freeze | After block D, `classical` accounts cannot spend until they register | Funds of lost or inactive keys are frozen; politically hard |
| P3. Opt-in freeze only | An account may freeze its own classical path before D | Same as P0 for people who do nothing |

Measurement owed before any choice: from an archive state at a pinned block, the sum of balances of addresses that have signed at least one transaction, as a fraction of supply (method only; no number exists yet; threat model Q2).

## 5. Wallets, custody and recovery

- Wallet and signer readiness is a gate (see governance proposal). A hardware wallet that cannot sign ML-DSA cannot hold a `pq-only` account; T24 (Ledger, smartcard, external signer) is unknown for PQ support.
- Key storage: the keystore v3 pattern (T23: scrypt or pbkdf2, AES-128-CTR, Keccak MAC) is symmetric and carries over; a PQ key is much larger (ML-DSA-65 secret key size is in FIPS 204 Table 2; the signature is 3,309 B and the public key 1,952 B, `../bandwidth-model.md` table 1).
- Recovery of a lost classical key after `pq-only` is impossible by design. A recovery scheme (guardians, time-locked recovery key) is an open question, not part of this proposal.
- Stateless schemes only for accounts. A stateful scheme (SP 800-208) needs a durable one-time-state reservation that a datadir restore must never roll back (`../../adr/ADR-010-signer-safety.md`, `xone-rs-pq-readiness` rule 6); that burden is unacceptable for ordinary wallets.

## 6. ecrecover-dependent contracts

- Precompile 0x01 stays byte-for-byte unchanged (T32); removing or changing it would break every contract that uses it.
- A new verification precompile (algorithm id, public key, message, signature) would let contracts verify PQ signatures. Its gas price needs the cost lab (the cost lab plan, `docs/pq/cost-lab-plan.md` from PR 281, Q1, Q3, Q4, Q5), not a guess.
- Contracts that verify ECDSA (permits, multisigs, bridges) have no migration path from the client; their owners must migrate (see [bridges-release-signing.md](bridges-release-signing.md)).

## 7. Budgets (arithmetic from the lab sizes; not measured on a network)

Source: `../bandwidth-model.md`. 1,000 TPS is 2,000 transactions per block; 3,000 TPS is 6,000.

| Item | ECDSA | ML-DSA-65 | Reading |
| --- | --- | --- | --- |
| Signature per transaction | 65 B | 3,309 B (50.9x) | the dominant cost |
| Public key (registry or first use) | not carried (recovered) | 1,952 B once per account | registry mode pays it once; no key per transaction |
| 1,000 TPS, signature only | 65 kB/s; 5.62 GB/day | 3.31 MB/s; 285.90 GB/day | table 2 |
| 3,000 TPS, signature only | 195 kB/s; 16.85 GB/day | 9.93 MB/s; 857.69 GB/day | table 2 |
| Verify CPU per block at 1,000 TPS | 307 ms (recover) | 189 ms | table 5, one core, lab p50 |
| Verify CPU per block at 3,000 TPS | 920 ms (recover) | 567 ms | table 5, one core, lab p50 |

DoS: a bad signature costs about as much to reject as a good one to accept (lab note 1: ML-DSA-65 94.5 us valid, 91.5 and 85.9 us invalid). So the pool needs a per-peer budget for PQ verification and a cheap pre-check of size and encoding (cost lab Q3 measures the cheapest reject). Gas, size and per-block limits are not proposed here; they need the lab runs.

## 8. Mixed versions and rollback

- Activation is a fork block per network, recorded as a new row in the generated fork table. Before activation the row is `Unscheduled` (the placeholder `99,999,999,999`, `spec/chainspec/unscheduled-sentinel.md`), which by construction never activates.
- A node that does not know the new rules rejects the first PQ transaction and forks off. This is why one client cannot activate it (README no-activation rule).
- Rollback across the activation block is a governance act (a second fork), not a client flag. The registry state is ordinary state; a datadir restore from before the fork replays the same blocks. A stateless scheme adds no restore hazard.
- Mode moves are one-way. A bug that registers a wrong key is not repairable by the account; a registry migration path must exist (open question).

## 9. Devnet vector plan (no activation)

All run on a private devnet only (chain 5551 or 551 profile, or a new private chain id); none on Apothem or mainnet.

| Vector | Expect |
| --- | --- |
| V-A1 registration with ECDSA and PQ proof of possession | account moves `classical` to `hybrid` |
| V-A2 registration with ECDSA only, or PQ only | rejected |
| V-A3 `hybrid` transaction with both signatures valid | accepted |
| V-A4 `hybrid` with one signature valid and one invalid, both orders | rejected (AND) |
| V-A5 `pq-only` account, ECDSA-only transaction | rejected |
| V-A6 algorithm id changed in the envelope to an unregistered or experimental id (`0xF000-0xFFFF` is reserved) | rejected, no fallback |
| V-A7 signature made for another domain (a V2 vote digest, T12) replayed as a transaction | rejected (domain tag) |
| V-A8 signature replayed on another chain id | rejected |
| V-A9 oversize and malformed encodings | rejected before full verification |
| V-A10 two clients on the same blocks, including a reorg across a mode move | identical state roots |

V-A10 needs at least two independent clients: the gate that no single client satisfies.

## 10. Open questions for the owner and reviewers

1. Which of options A, B, C (direction: B).
2. Which legacy-balance policy, P0 to P3, or a different one.
3. Whether `pq-only` is allowed at all in the first activation, or only `hybrid`.
4. Address strength: the 160-bit hash of the public key (threat model Q1) has no cited quantum-search source yet.
5. Owner of the registry contract and its upgrade authority (T21 is unknown).
