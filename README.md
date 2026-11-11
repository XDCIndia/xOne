# xOne

XDC execution-client rebuild in Rust. The build backlog is in [#51](https://github.com/XDCIndia/xOne/issues/51); [#57](https://github.com/XDCIndia/xOne/issues/57) defines readiness for individual deliverables and the evidence needed to close their parent issues.

## Status

Source reviewed: `876bb8e9679d3df67aabccea32cfe7047a3284c3`, the merge of [PR #93](https://github.com/XDCIndia/xOne/pull/93). The repository now contains runtime crates, a simulator and a runnable development follower. It has not completed public-network or mainnet qualification.

| Area | Merged code | Remaining qualification |
| --- | --- | --- |
| Types and rules | `xone-primitives`, `xone-chainspec`: header encodings, genesis and generated fork rules | Complete each network's rule, negative-input and corpus gates |
| Execution and state | `xone-evm`, `xone-executor`, `xone-state`: revm integration, XDC execution and trie/state views | Full historical replay and runtime integration of all active rules |
| Consensus and signing | `xone-consensus`, `xone-signer`, `sim/`: V1/V2 rules, signing interfaces and simulation | Live V2 epoch context, durable safety and producer gates |
| Network and sync | `xone-net`, `xone-sync`, `xone-snap`: wire protocols, sync drivers and snap components | Complete production wiring, interoperable sync and restart proof |
| Node and operator services | `xone-db`, `xone-node`, `xone-builder`, `xone-rpc`: storage contracts, service wiring, pool and RPC | Durable store/stage checkpoints, coherent import/reorg behavior and supported operator surfaces |

[Replay records](conformance/harness/RUNS.md) report 432,000 Apothem blocks from genesis and separate diagnostic pivot windows. [The first-follow guide](docs/ops/first-follow.md) records a short private-loopback run. These records describe their source, mode and limitations; they do not close the full-history, real-disk failure, public follower or producer gates. At the reviewed pin, node/sync contexts still refuse V2 checkpoint blocks without the required gap snapshots, and `FileStore` does not persist staged-sync rows.

PR #93 reports local tests on its integration tree. Its hosted CI run failed; this review has not independently established the reported billing cause or rerun those product tests. See the [review snapshot](.audit/current-review.json) for the evidence boundary and the [execution plan](docs/plans/2026-11-11-execution-plan.md) for next work.

## Build and inspect

The Rust toolchain is pinned in `rust-toolchain.toml`; rustup installs it when needed.

```sh
cargo build --locked --release -p xone
cargo test --locked
./target/release/xone --help
./target/release/xone --version
./target/release/xone genesis-hash --network mainnet
./target/release/xone dumpconfig
```

These commands build, test or inspect the client. For the private-loopback follower setup, use the source and limitations in [first-follow](docs/ops/first-follow.md). Keep the final host, network and signing qualification gates in [#51](https://github.com/XDCIndia/xOne/issues/51).

## Rules and evidence

The oracle pin is `6fc8e8c2088b7f18264f5663036669d04fa7becf` in `conformance/PIN`. The fork table in `crates/xone-chainspec/generated/forks.json` is generated from that geth-XDC source. Regeneration and comparison require read access to the private oracle repository:

```sh
conformance/fetch-geth.sh
cd conformance/exporter
go run . -check
```

A check that cannot read the pinned oracle is unqualified. Parse tests of the committed table do not replace the oracle comparison. Keep source pins fixed for a work item; change a pin through a reviewed update with the affected regression evidence.

- [Execution plan and verification loop](docs/plans/2026-11-11-execution-plan.md)
- [Audit history and current source review](.audit/README.md)
- [Architecture and post-quantum readiness review](docs/reviews/2026-11-10-architecture-scale-pq-review.md)
- [Documentation guide](docs/README.md)

Post-quantum readiness and protocol migration are separate workstreams. Their research does not establish quantum resistance for the current XDC protocol or authorize a network change.
