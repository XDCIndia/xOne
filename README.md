# xOneRS

xOneRS is the Rust client of the xOne family (xOneRS, xOneTS, xOneC, xOnePy): an XDC execution client rebuilt in Rust. The binary and crate names stay `xone`, `xone-*`. The build backlog is [#51](https://github.com/XDCIndia/xOneRS/issues/51); [#57](https://github.com/XDCIndia/xOneRS/issues/57) defines readiness for each deliverable.

## Status

This section describes `main`. Evidence for each claim is in the linked guide; a claim without a guide is not yet proven.

| Area | What runs on `main` | Guide |
| --- | --- | --- |
| Follow net2040 (chain 2040) | Full sync from genesis on its own MDBX state; roots, receipts, gas and bloom checked per block; restart resumes | [net2040-follow](docs/ops/net2040-follow.md) |
| Follow Apothem (chain 51) | Full sync from genesis; bootnodes from `ops/bootnodes/apothem.txt` (the node has no built-in boot list) | [apothem-follow](docs/ops/apothem-follow.md) |
| Sync modes | `--syncmode full` (default), `snap` and `fast` (same mode) from an xOne peer over snap/1 + xpre/1; `--gcmode archive` is refused | [net2040-sync-modes](docs/ops/net2040-sync-modes.md) |
| Snapshots, minimal node | `xone snapshot export` and `import`; `--minimal` keeps only what consensus reads | [snapshot](docs/ops/snapshot.md) |
| Disk state | `--state.disk` keeps the head state on disk instead of in memory | [disk-state](docs/ops/disk-state.md) |
| Producer (`--mine`) | Runs on private devnets (V1 into V2, kill -9 safe) and is allowed on Apothem; refused on mainnet (chain 50). A mint on a public network needs the owner's approval | [first-mint-local](docs/ops/first-mint-local.md), [gate-d-rehearsal](docs/ops/gate-d-rehearsal.md), [keys](docs/ops/keys.md) |
| Private-loopback follow | A follower of a private geth-XDC chain (historical run) | [first-follow](docs/ops/first-follow.md) |
| Release and operations | Local release script (GitHub Actions is on a billing hold); systemd and Docker install | [release](docs/ops/release.md), [release-rehearsal](docs/ops/release-rehearsal.md), [ops/README](ops/README.md), [install](ops/install.md), [upgrade-rollback](ops/upgrade-rollback.md) |

Not qualified: a mainnet follow or mint, a 72 h producer soak, and full-history replay on real disk. [Replay records](conformance/harness/RUNS.md) list each replay with its range, mode and limits. The [execution plan](docs/plans/2026-11-11-execution-plan.md) holds the next work and [the audit folder](.audit/README.md) the evidence boundary.

## Architecture

Crates in `crates/`, lowest layer first (layering is checked by `ci/check-layering.sh`):

- Types and rules: `xone-primitives` (header encodings), `xone-chainspec` (genesis and fork tables)
- Execution and state: `xone-evm`, `xone-executor`, `xone-state`
- Consensus and signing: `xone-consensus` (XDPoS V1 and V2), `xone-signer` (keystore, anti-equivocation record)
- Network and sync: `xone-net` (RLPx, eth), `xone-snap` (snap/1 client and server), `xone-sync`
- Node: `xone-db` (MDBX storage), `xone-node` (services, config, datadir, producer), `xone-builder`, `xone-rpc`

The binary is `bin/xone`. Rule specs are in `spec/`, decisions in `docs/adr/`, flags in [docs/cli/flags.md](docs/cli/flags.md).

## Quick start: follow Apothem

```sh
cargo build --locked --release -p xone     # toolchain is pinned in rust-toolchain.toml
./target/release/xone --version
./target/release/xone --network apothem --datadir ./apothem-data \
    --bootnodes "$(paste -sd, ops/bootnodes/apothem.txt)" --http
```

The node dials the bootnodes, imports every block on its own state and serves JSON-RPC. The [Apothem guide](docs/ops/apothem-follow.md) gives the full command, ports, metrics and the SkyNet report. For net2040 use [net2040-follow](docs/ops/net2040-follow.md). Other inspection commands: `xone --help`, `xone genesis-hash --network mainnet`, `xone dumpconfig`. Run the tests with `cargo test --locked`.

## Rules and evidence

The oracle pin is in `conformance/PIN`. The fork table `crates/xone-chainspec/generated/forks.json` is generated from that geth-XDC source. Regeneration and comparison need read access to the private oracle repository:

```sh
conformance/fetch-geth.sh
cd conformance/exporter
go run . -check
```

A check that cannot read the pinned oracle is not qualified; in CI the `fork-table-drift` job then reports "not qualified" instead of a result. Parse tests of the committed table do not replace the comparison. Change a pin only through a reviewed update with the regression evidence.

- [Execution plan and verification loop](docs/plans/2026-11-11-execution-plan.md)
- [Audit history and current source review](.audit/README.md)
- [Architecture and post-quantum readiness review](docs/reviews/2026-11-10-architecture-scale-pq-review.md)
- [Documentation guide](docs/README.md)

Post-quantum readiness is a separate workstream; its research does not establish quantum resistance for the current XDC protocol or authorize a network change.
