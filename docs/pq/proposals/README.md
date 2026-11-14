# PQ track B research proposals

Issues [#19](https://github.com/XDCIndia/xOneRS/issues/19) (XA-14) and [#20](https://github.com/XDCIndia/xOneRS/issues/20) (XA-15). **Research only.** These documents change no code, no consensus rule and no wire byte, and they activate nothing. No production activation without network governance, independent audits and a multi-client devnet ([governance-activation.md](governance-activation.md)). No PQ scheme or library is chosen; where a scheme is named it is a candidate, and every library is lab input until reviewed ([`../implementations.md`](../implementations.md)).

Inputs: [`../inventory.md`](../inventory.md) (rows `T..`, `P..`), [`../threat-model.md`](../threat-model.md) (sections 5 to 9), [`../standards.md`](../standards.md), [`../bandwidth-model.md`](../bandwidth-model.md), [`../lab-results-2026-11-11.md`](../lab-results-2026-11-11.md). Pins: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf`; xOne `origin/main` `a99fcf7`.

| Issue | Proposal | Surface | Changes consensus or wire |
| --- | --- | --- | --- |
| #19 | [accounts-wallets.md](accounts-wallets.md) | account authentication, wallets, legacy balances, ecrecover contracts | consensus |
| #19 | [validators-qc-tc.md](validators-qc-tc.md) | validator keys, votes, QC, TC, registry | consensus |
| #19 | [governance-activation.md](governance-activation.md) | gate, activation mechanics, order | none (process) |
| #20 | [transport-identity.md](transport-identity.md) | RLPx hybrid key establishment, identity, discovery limits | wire only |
| #20 | [history-checkpoints.md](history-checkpoints.md) | anchors, manifests, checkpoint attestation, light clients | manifest part compatible; attestation coordinated |
| #20 | [bridges-release-signing.md](bridges-release-signing.md) | bridges, governance and custody, release and snapshot signing | release part compatible; the rest outside the client |

Every proposal has the same sections: problem, assumptions, proposal, downgrade protection, size and cost budget (arithmetic from the lab sizes, not measured on a network), mixed-version and rollback behaviour, a devnet vector plan, and open questions for the owner. Hybrid rule everywhere: where protection is needed, both signatures are required (AND), never either.
