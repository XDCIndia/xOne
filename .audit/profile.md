# xOne audit profile (2026-11-11)

## Target and pins

| Repository | Ref | Full SHA |
|---|---|---|
| XDCIndia/xOne (target, private) | `main` | `2e77a4cf516c23a5fd5d7c5421a41d7a58a02dd1` |
| XDCIndia/go-ethereum (compatibility oracle) | `main` | `6fc8e8c2088b7f18264f5663036669d04fa7becf` (2026-11-10) |
| XDCIndia/go-ethereum xdc.20 worktree (Apothem schedule) | local | `54081f042211292a4c21153b5a8f857787a0cfa5` (ancestor of the main pin; same Apothem rows) |
| XDCIndia/xOneGo (current xOne line) | local `xone-anchor` | `8bfacb137ec059cebf8807b2411cb36da70acc5d` |
| reth-xdc | local `/mnt/data/tmp/reth-apothem-spec` | `49c5719f1f8ed27426333996ba360cc57862bd12` |
| erigon-xdc | local `epochfix` / `hdrfast` | `3b851d75dcf6d72ff8662be90f95ae4912956ba2` / `00a978ab5db60912004071e9316367bc86d6ee83` |
| nethermind fork | XDCIndia/xdc-nethermind-private | `d9ca4e4acd7b53e5ad0645c3d12e3c6e40175447` |
| besu fork | XDCIndia/besu-xdc, XDCIndia/besu | code search returned no hits; not inspected (access/index gap, not absence) |

## Product goal

A Rust XDC execution client in `XDCIndia/xOne` (binary `xone`) that is XDC-compatible from genesis, verified independently of a single oracle, durable and signer-safe, with workload-scoped latency/resource targets. PQ migration is a parallel research track (Track Q) under explicit protocol constraints.

## What exists at the pin

- No client code. The repository holds `README.md`, `docs/` (architecture review, sources, master prompt), a PDF, and eight `xone-rs-*` skills under `.claude/skills/` (PR #2, merged). PR #4 (skills revision, two new skills) is open.
- The rebuild plan is issue #1 with three comments: skills note, "Addendum: Rust framework survey", and "Revision 1: external architecture review applied" (2026-11-10 18:45 UTC). Revision 1 supersedes the plan where it conflicts and leaves issue filing to this audit.
- Every proposed crate, ADR, harness, SLO and gate is planned, not deployed.

## Identity and failure domains

- xOne-rs **replaces** the xOneGo line (plan B5: xOneGo gets wedge fixes only and retires after the safety gates; the owner moves validator `0x68a2`). It is called the "seventh client", but during transition it is a sixth implementation family plus a successor binary.
- Shared dependency domains: revm (reth-xdc used revm until reth moved to evm2; xOne plans revm 43.x), alloy-trie/alloy primitives, secp256k1 crypto, RLPx/devp2p, the Go oracle built from the same geth-XDC line that also produces the reference client. A shared revm or oracle defect can hit more than one client.

## Users and modes

Operators (follower, producer), SkyNet monitoring, parity scripts, validator owners. Networks: Apothem (51), mainnet (50), net2040 (private). Modes planned: full, anchored minimal, snapshot bootstrap, producer.

## Trust boundaries

1. P2P input (eth/100, eth/101, `xdpos2`, snap/1) - untrusted.
2. Snapshot/manifest downloads - untrusted bytes; the anchor must be authenticated.
3. RPC clients - untrusted, need quotas.
4. Validator key and signer - highest privilege; must be isolated behind the safety unit.
5. Oracle/exporter - test-only trust; never a runtime dependency.
6. Native/unsafe dependencies (MDBX, crypto) - inside the trusted computing base.

## Evidence limits

Campaign figures in go-ethereum#1950 are host observations (cgroup memory includes page cache), not reproduced benchmarks. Nothing was built, run or benchmarked in this audit.
