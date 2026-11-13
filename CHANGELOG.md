# Changelog

## Unreleased

- **`--state.disk`, part 1 of 3 (issue #145).** The head state is read from the MDBX tables behind
  bounded caches instead of a copy of every row in memory; checkpoints and the snap install write no
  state rows with the flag. Off by default. `docs/ops/disk-state.md`.

## v0.1.0 - 2026-11-12

First release of xOne, the Rust client for XDC Network. The workspace version is 0.1.0.

### What is in it

- **Client.** Rust crates for primitives, chainspec, consensus (XDPoS V1 and V2), EVM, state,
  executor, database, networking, snap sync, node, RPC, signer and block builder. One binary, `xone`.
  The version stamp (`xOne/v<semver>-<commit8>-<DDMMYYYY>/<os>-<arch>/rustc<version>`) comes from one
  function that `xone --version`, `web3_clientVersion`, the P2P hello and ethstats all use.
- **Sync modes.** Full, snap, fast (an alias of snap, as in geth-XDC), minimal (snapshot import with
  a pinned anchor) and archive. Snap and fast verify the pivot state root and the epoch-switch anchor
  before they execute.
- **Producer.** XDPoS V2 block producer: votes, timeouts, QC and TC, sibling block on a head with no
  QC, signer with a safety record.
- **RPC.** Eth, net, web3 and XDPoS namespaces. Follower nodes answer a typed error below their
  retained window.

### Proof on a live network (net2040, 2026-11-12)

Build `xOne/v0.1.0-85a75a23-12112026`. Details: `docs/ops/net2040-sync-modes.md`, section "Resync
2026-11-12, third run", and issue #47.

- Every node type (snap, fast, minimal, archive, full) synced to the tip and equal to the archive
  node on blocks, receipts, balances, code, storage and masternodes inside its window.
- Each of snap, fast and minimal sealed 3 of 3 eligible turns on a borrowed seat, each block equal on
  the archive node:

  | Node | Seat | Seals |
  | --- | --- | --- |
  | snap | 0xc33a | 780010, 780023, 780036 |
  | fast | 0x00ce | 780063, 780076, 780089 |
  | minimal | 0x00ce | 780114, 780127, 780140 |

- The full node held seat 0xa0ea for the whole day with 0 missed turns.

Three seals per node are a smoke test, not reliability evidence.

### Known gaps

- #118 signer: the ADR-010 quiet window is 0 on every chain except 50 and 51, so the fence is
  inactive on net2040 and private chains.
- #119 net: a snap-synced node reports total difficulty counted from its pivot in Status, so peers
  rank it last.
- #120 conformance: EVM oracle gaps (weak difficulty-56.4M vector, BALANCE mask undetectable, missing
  Amsterdam, TRC21-token, BLS-P256 and EIP-7702 vectors).
- #121 rpc: the typed unsupported-history refusal reports the head as pivot on snap, fast and
  minimal nodes.
- #122 rpc: `eth_subscribe` newHeads and logs return an id but deliver no notification.
- #123 ethstats: a node started with `--minimal` reports syncMode "full".
- #128 producer: no sibling when the head's round is at or above ours; the taken-off block's
  transactions are not re-injected (Gate D case A1).
