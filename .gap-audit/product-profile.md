# Product profile: xOneRS

Audited source: `XDCIndia/xOneRS` `main` at `8b047da` (2026-11-14). Paths are relative to the repository root.

## Category

XDC Network execution client with built-in XDPoS consensus (a "full node" in the geth-XDC sense): one Rust binary that syncs, validates, executes, stores and serves the XDC chain and can produce blocks as a masternode. Ethereum-family execution client; XDC-specific consensus (XDPoS V1 epochs and V2 HotStuff-style BFT).

## Core capabilities (with evidence)

1. XDC header and transaction encodings, seal hashes, V2 consensus types, wide balances: `crates/xone-primitives/src/{header.rs,seal.rs,v2.rs,wide_balance.rs}`.
2. Chain specs and fork table for mainnet (50), Apothem (51), devnet and net2040, generated from geth-XDC: `crates/xone-chainspec/src/networks.rs`, `crates/xone-chainspec/generated/forks.json`.
3. Custom genesis / private chains (`--override.genesis`, `--chainspec`): `crates/xone-chainspec/src/genesis.rs`, `docs/cli/flags.md`.
4. XDPoS V1 consensus (epochs, snapshots, double validation, checkpoint masternodes, penalties): `crates/xone-consensus/src/v1/`, `spec/consensus/v1-*.md`.
5. XDPoS V2 BFT (votes, timeouts, QC/TC, SyncInfo, epoch switch, gap snapshot, forensics): `crates/xone-consensus/src/v2/`, `spec/consensus/v2-*.md`.
6. Rewards and penalties (V1, V2 tiered/proportional): `crates/xone-consensus/src/{reward,penalty}`.
7. EVM on revm with XDC hooks, per-fork precompiles, fixed base fee, TRC21 fee routing, special transactions: `crates/xone-evm/src/{handler.rs,precompiles.rs,special.rs,fees.rs}`.
8. Block execution, sender recovery pool, block access lists (Amsterdam): `crates/xone-executor/src/{lib.rs,tx.rs,bal.rs}`.
9. State: flat state, MPT commitment, changesets for unwind, disk-backed state option: `crates/xone-state/src/{flat.rs,mpt.rs,trie.rs,changeset.rs}`, `--state.disk`.
10. Storage: MDBX store, static segments, manifest, crash recovery, fault injection, signer safety record: `crates/xone-db/src/{mdbx.rs,segments.rs,recover.rs,fault.rs,safety.rs}`, `docs/adr/ADR-005-storage-and-publication.md`.
11. Staged full sync (headers, bodies, senders, execute, finish) and live follow: `crates/xone-sync/src/{stages,follow.rs}`.
12. Snap sync (snap/1 client and server, range proofs, xpre/1 pre-pivot data, gap-RPC proof checks): `crates/xone-snap/src/`, `crates/xone-node/src/services/snapsync.rs`.
13. Bootstrap: snapshot export/import with pins and anchored minimal mode: `bin/xone/src/cmd/snapshot.rs`, `spec/bootstrap/`.
14. Chain import/export of RLP block files: `bin/xone/src/cmd/{import.rs,export.rs}`, `crates/xone-sync/src/{import.rs,export.rs}`.
15. Fork choice, reorg, set-head, restart recovery: `crates/xone-sync/src/chain/`, `spec/chain/`.
16. devp2p: discv4 (XDC ping), RLPx, eth/100 and eth/101, BFT message gossip, peer scoring, static and trusted peers: `crates/xone-net/src/`, `crates/xone-node/src/enode.rs`.
17. Txpool: geth legacypool port, special-tx admission, propagation, eviction: `crates/xone-builder/src/txpool/`.
18. Block producer: leader schedule, build, seal V1/V2, sign transactions, slot timing: `crates/xone-builder/src/producer/`.
19. Keys: keystore v3 read, remote signer with fencing, node key, secret zeroization: `crates/xone-signer/src/`.
20. JSON-RPC over HTTP, WebSocket and IPC with quotas, classes, deadlines and load shedding: `crates/xone-rpc/src/{server.rs,transport.rs,policy.rs}`, `spec/rpc/scope-adr-008.md`.
21. RPC namespaces: eth subset (29 methods), net, web3, txpool (status/content/inspect), admin (5), debug (raw header/block, callTracer/prestateTracer traces), XDPoS (14), XDC eth extensions (7), subscriptions newHeads/logs/newPendingTransactions: `crates/xone-rpc/src/*.rs`.
22. Observability: Prometheus metrics, /livez and /readyz, ethstats, SkyNet heartbeat, budgeted rotating logs: `crates/xone-node/src/observe/`.
23. CLI and config compatible with geth-XDC/join.sh flags, TOML config, dumpconfig, genesis-hash: `bin/xone/src/flags.rs`, `docs/cli/flags.md`.
24. Operator packaging: Dockerfile, systemd unit, install and upgrade/rollback runbooks, bootnode lists: `Dockerfile`, `ops/`.
25. Conformance: replay harness against the geth-XDC archive, golden vectors, fuzzing, simulator: `conformance/`, `fuzz/`, `sim/`.
26. Post-quantum readiness research (inventory, threat model, lab): `docs/pq/`.

## Tech stack, licence, target users

- Rust (pinned toolchain), revm, alloy-primitives/rlp/trie, libmdbx, tokio for networking; Go only in oracle/exporter tooling. Licence GPL-3.0-or-later (`Cargo.toml`).
- Target users: XDC node operators (full, snap/fast, minimal, archive-style followers), masternode/validator operators, RPC providers, explorers, dapp integrators, the XDCIndia multi-client fleet (SkyNet).

## Search keywords and synonyms

ethereum execution client; ethereum client rust; ethereum full node; ethereum node implementation; evm blockchain client; XDC network client; XDC blockchain; XinFin node; XDPoS; XDPoS v2; HotStuff BFT EVM chain; DPoS EVM client; PoSA client; proof of authority ethereum fork; geth fork; reth fork; revm node; devp2p; snap sync; staged sync; MDBX blockchain storage; JSON-RPC ethereum node; masternode client; validator client EVM; archive node; execution layer client; ethereum client go; ethereum client java; ethereum client dotnet; ethereum client nim.
