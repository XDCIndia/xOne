# xpre/1: preimages for an address-keyed state

Code: `xone_snap::preimage` (codec), `xone_snap::server::Server::preimages`, `xone_net::rlpx::Sub::Xpre`.

snap/1 transfers state by hashed key and carries no preimages (`eth/protocols/snap/protocol.go`). The xOne
state is keyed by address and slot (ADR-005 `PlainAccountState`, `PlainStorageState`, the checkpoints, the
changesets), so a node that snap-syncs needs the address behind each `keccak(address)` and the slot behind
each `keccak(slot)`. xpre/1 is an additive, xOne-only capability that serves them.

## Capability

Name `xpre`, version 1, two codes. Codes follow the eth codes in capability name order, as geth's
`matchProtocols` assigns them (`p2p/peer.go:438-459`): with eth/100 or eth/101 (229 codes), snap/1 is at
eth-relative 229..236 and xpre/1 at 237..238 when both are shared, or at 229..230 when snap/1 is not. A peer
that does not offer xpre/1 (every geth node) never receives one of its frames, and its other codes do not move.
Every xOne node offers snap/1 and xpre/1 (`xone_net::peers::Config::subs`).

## Messages

| code | message | body |
| --- | --- | --- |
| 0x00 | GetPreimages | `[id, [hash, ...]]`, at most 4,096 hashes decoded, at most 1,024 served |
| 0x01 | Preimages | `[id, [preimage, ...]]`: the known ones, in request order, an unknown one skipped |

Decoding is strict (one canonical RLP value, no trailing byte, list caps). A server answers from the rows of the
snapshot it serves (the pivot's), so the preimages are those of the state it serves.

## Trust

A preimage proves itself: the client keeps a preimage only if its keccak is a hash it asked for, with 20 bytes for
an account and 32 for a slot, and then rebuilds the state root from the plain rows and compares it with the pivot
header. A peer can withhold (the client retries, then fails with `SnapSyncError::Preimage(hash)`), never forge.

## Consequence

Since the hashed-key store (ADR-005 amendment 2026-11-12, issue #134) a snap sync needs no preimage: the state is
keyed by `keccak(address)` and `keccak(slot)` like snap/1, and the client does not ask for xpre/1. An xOne node
still offers xpre/1 and answers from its `Preimages` table, which execution and installs fill; a hash it has not
seen is skipped. The text above describes the capability; the client no longer depends on it.
