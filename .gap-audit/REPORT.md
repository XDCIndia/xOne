# Feature-gap audit report: xOneRS

Date: 2026-11-14. Audited source: `XDCIndia/xOneRS` main `8b047da`. Method: the owner's five-phase brief (product profile, competitor discovery and scoring, feature matrix, gap ledger, issue loop, expansion until a pass adds nothing). Competitor code was read from shallow clones under `.gap-audit/refs/` (not committed); no competitor code was built or run.

Files: [product-profile.md](product-profile.md), [competitors.json](competitors.json), [feature-matrix.md](feature-matrix.md), [gap-ledger.json](gap-ledger.json), [bodies/](bodies/).

## Competitors

### Selected (score 60+)

| Repo | Score | Overlap/Activity/Adoption/Access | Reason |
|---|---|---|---|
| `XinFinOrg/XDPoSChain` | 86 | 48/20/3/15 | Canonical public XDC client (XDPoS V1/V2, XDC RPC extensions). GitHub fork of go-ethereum that adds the whole XDC protocol, so the fork rule does not apply. |
| `ethereum/go-ethereum` | 80 | 30/20/15/15 | Upstream of geth-XDC and XDPoSChain; reference for RPC, tracing, CLI and operator tooling. |
| `bnb-chain/bsc` | 79 | 32/20/12/15 | geth fork with PoSA (Parlia) and fast-finality votes: the closest DPoS/BFT analogue to XDPoS; adds block pruning and vote tooling over geth. |
| `paradigmxyz/reth` | 78 | 28/20/15/15 | Rust execution client; same language and storage engine (MDBX) as xOneRS. |
| `0xPolygon/bor` | 78 | 31/20/12/15 | geth fork for a validator-set PoS chain; adds history pruning and sequencer tooling over geth. |
| `besu-eth/besu` | 77 | 30/20/12/15 | Java client with IBFT2/QBFT BFT engines (round-based BFT like XDPoS V2), storage subcommands, OTel. |
| `erigontech/erigon` | 75 | 28/20/12/15 | Staged-sync client on MDBX; trace_ and ots_ namespaces, prune modes. |
| `NethermindEth/nethermind` | 75 | 28/20/12/15 | .NET client with AuRa/Clique PoA, trace/ots modules, snapshot download. |
| `lambdaclass/ethrex` | 69 | 25/20/9/15 | Minimal Rust client; L1 RPC, debug and admin surface comparable to ours. |
| `status-im/nimbus-eth1` | 68 | 24/20/9/15 | Resource-restricted execution client; filters, GraphQL, minimal-hardware focus. |

### Rejected

| Repo | Score | Reason |
|---|---|---|
| `XinFinOrg/XDC-Subnet` | 83 | Derived from XDPoSChain for XDC subnets; adds subnet-only features and nothing operator-facing for mainnet/Apothem nodes over its selected parent. |
| `bnb-chain/reth-bsc` | 68 | reth SDK extension that adds Parlia only; its operator surface is reth's (selected); Parlia features skipped (SKIP-13). |
| `ethereumjs/ethereumjs-monorepo` | 57 | Score 57: libraries only; the client package is no longer in packages/ (block, blockchain, evm, vm, devp2p, ...). |
| `ronin-chain/ronin` | 56 | Score 56: DPoS geth fork, last commit 7 months ago, low adoption. |
| `gnosischain/reth_gnosis` | 56 | Score 56: reth extension (AuRa); no licence file; adds nothing operator-facing over reth. |
| `loocapro/reth-bsc` | 58 | Score 58: predecessor of bnb-chain/reth-bsc; superseded. |
| `alt-research/eth2030` | 50 | Score 50: research client for the 2030 roadmap; licence unclear. |
| `erigontech/silkworm` | 46 | Score 46: C++ execution library, partial node, last commit 18 months ago. |
| `akula-bft/akula` | 44 | Score 44: archived Rust client. |
| `ethereum/aleth` | 42 | Score 42: archived C++ client. |
| `PegaSysEng/pantheon` | 26 | Score 26: archived predecessor of Besu. |
| `XinFinOrg/XinFin-Node` | 36 | Score 36: Docker set-up scripts for the XDC client, not a client. Docs-only reference for operator set-up. |
| `evmts/xvi` | 43 | Score 43: early-stage client. |
| `mana-ethereum/mana` | 23 | Score 23: unmaintained Elixir node. |
| `stateless-consensus/phant` | 38 | Score 38: stateless research client. |
| `weiihann/chibi` | 48 | Score 48: partial-state node, narrow scope. |
| `vicnaum/shinode` | 36 | Score 36: history-only node. |
| `mmsaki/zeth` | 36 | Score 36: toy Zig client. |
| `circlefin/malaketh-layered` | 46 | Score 46: consensus engine driving an EL over Engine API; not a client. |
| `Nethereum/Nethereum` | 50 | Score 50: client library, not a node. |
| `ethpandaops/benchmarkoor` | 41 | Score 41: benchmarking tool, not a client. |
| `XDCIndia/besu-xdc` | 68 | Score 68 but excluded by the fork rule: GitHub fork of besu-eth/besu (selected) for XDC; XDC behaviour is taken from XDPoSChain and the geth-XDC oracle; same owner as xOneRS. |
| `XDCIndia/go-ethereum` | 73 | Our conformance oracle (conformance/PIN), private, same owner: used as the parity reference, not as a competitor. Its XDC-only RPC methods are cited in GAP xdcext. |
| `XDCIndia in-house XDC forks of erigon, reth and nethermind, and xOneGo/xOneTS/xOneC/xOnePy` | - | Same-owner sibling clients (private). Their parents are selected; the xOne family shares this repository's specs. Not competitors. |
| `(commercial) hosted RPC providers: Alchemy, QuickNode, Ankr, Infura` | - | Docs-only references: they host the open clients above; no closed-source XDC or Ethereum execution client leads the category, so no separate closed feature set was scored. |

## Gaps and issues

Priority rule: P0 = a missing node type (archive); P1 = 8+ of 10 selected competitors, or present in the canonical XDC clients; P2 = 5-7; P3 = 1-4.

| ID | Issue | Priority | Area | Seen in | Title |
|---|---|---|---|---|---|
| GAP-0001 | [#152](https://github.com/XDCIndia/xOneRS/issues/152) | P0 | storage | 9/10 | Archive node mode: keep historical state for every block (--gcmode archive) |
| GAP-0002 | [#153](https://github.com/XDCIndia/xOneRS/issues/153) | P1 | rpc | 10/10 | Polling log/block/pending filters over HTTP (eth_newFilter, eth_getFilterChanges and siblings) |
| GAP-0003 | [#154](https://github.com/XDCIndia/xOneRS/issues/154) | P1 | tracing | 10/10 | Struct (opcode) logger: the default tracer of debug_trace* |
| GAP-0004 | [#155](https://github.com/XDCIndia/xOneRS/issues/155) | P1 | rpc | 10/10 | eth_createAccessList |
| GAP-0005 | [#156](https://github.com/XDCIndia/xOneRS/issues/156) | P1 | rpc | 10/10 | Standard read-only RPC methods still missing (eth_maxPriorityFeePerGas, eth_getHeaderBy*, eth_getRawTransaction*, eth_pendingTransactions, uncle-by-index, txpool_contentFrom, rpc_modules, syncing subscription) |
| GAP-0006 | [#157](https://github.com/XDCIndia/xOneRS/issues/157) | P1 | rpc | 9/10 | eth_getProof: account and storage Merkle proofs |
| GAP-0007 | [#158](https://github.com/XDCIndia/xOneRS/issues/158) | P1 | rpc | 9/10 | State and block overrides for eth_call and eth_estimateGas |
| GAP-0008 | [#159](https://github.com/XDCIndia/xOneRS/issues/159) | P1 | rpc | 9/10 | eth_simulateV1 and multi-call bundles (eth_callMany / eth_callBundle) |
| GAP-0009 | [#160](https://github.com/XDCIndia/xOneRS/issues/160) | P1 | rpc | 9/10 | debug diagnostics: getBadBlocks, getRawReceipts, getRawTransaction, setHead, chaindbProperty |
| GAP-0010 | [#161](https://github.com/XDCIndia/xOneRS/issues/161) | P1 | tracing | 8/10 | debug_traceCall: trace a call on any block's state with overrides |
| GAP-0011 | [#162](https://github.com/XDCIndia/xOneRS/issues/162) | P1 | rpc | 8/10 | debug state inspection: storageRangeAt, accountRange, dumpBlock, getModifiedAccountsBy* |
| GAP-0012 | [#163](https://github.com/XDCIndia/xOneRS/issues/163) | P1 | rpc | 8/10 | Admin and runtime operations: start/stop RPC listeners, exportChain/importChain, peerEvents, removeTrustedPeer, runtime log level |
| GAP-0013 | [#164](https://github.com/XDCIndia/xOneRS/issues/164) | P1 | observability | 8/10 | Structured JSON log output (--log.format json) |
| GAP-0014 | [#165](https://github.com/XDCIndia/xOneRS/issues/165) | P1 | txpool | 8/10 | Txpool operator controls: --txpool.* limits, local accounts and a persistent local-transaction journal |
| GAP-0015 | [#166](https://github.com/XDCIndia/xOneRS/issues/166) | P1 | cli | 8/10 | Developer mode: single-node instant XDPoS chain (xone --dev) |
| GAP-0016 | [#167](https://github.com/XDCIndia/xOneRS/issues/167) | P1 | p2p | 8/10 | External address detection and NAT traversal: --nat any|upnp|pmp, advertised endpoint from discovery |
| GAP-0017 | [#168](https://github.com/XDCIndia/xOneRS/issues/168) | P1 | xdc-rpc | 1/10 | XDC RPC extensions served by geth-XDC and XDPoSChain: eth_getRewardByHash, eth_getStakerROI*, eth_getTransactionAndReceiptProof, eth_getAccountInfo, eth_getCheckpointFromEpoch, XDPoS_getV2BlockByHash and siblings |
| GAP-0018 | [#169](https://github.com/XDCIndia/xOneRS/issues/169) | P2 | tracing | 7/10 | Additional native tracers: 4byteTracer, flatCallTracer, muxTracer, noopTracer |
| GAP-0019 | [#170](https://github.com/XDCIndia/xOneRS/issues/170) | P2 | tracing | 7/10 | JavaScript tracers for debug_trace* |
| GAP-0020 | [#171](https://github.com/XDCIndia/xOneRS/issues/171) | P2 | rpc | 7/10 | RPC safety flags: --rpc.gascap, --rpc.txfeecap, --rpc.evmtimeout and quota flags |
| GAP-0021 | [#172](https://github.com/XDCIndia/xOneRS/issues/172) | P2 | rpc | 6/10 | GraphQL endpoint (EIP-1767) |
| GAP-0022 | [#173](https://github.com/XDCIndia/xOneRS/issues/173) | P2 | rpc | 6/10 | eth_sendRawTransactionSync (EIP-7966): send and wait for the receipt |
| GAP-0023 | [#174](https://github.com/XDCIndia/xOneRS/issues/174) | P2 | observability | 6/10 | Profiling endpoint: --pprof is accepted but nothing listens |
| GAP-0024 | [#175](https://github.com/XDCIndia/xOneRS/issues/175) | P2 | p2p | 6/10 | --netrestrict: restrict peers to CIDR ranges |
| GAP-0025 | [#176](https://github.com/XDCIndia/xOneRS/issues/176) | P2 | cli | 6/10 | Database inspection and maintenance CLI: xone db stats / inspect / get / compact and offline unwind |
| GAP-0026 | [#177](https://github.com/XDCIndia/xOneRS/issues/177) | P2 | storage | 6/10 | History pruning: configurable retention of bodies, receipts and logs, and pruning of an existing datadir |
| GAP-0027 | [#178](https://github.com/XDCIndia/xOneRS/issues/178) | P2 | p2p | 6/10 | Bootnode-only mode and devp2p tooling (discovery-only node, ENR/enode tools, crawler) |
| GAP-0028 | [#179](https://github.com/XDCIndia/xOneRS/issues/179) | P2 | observability | 5/10 | OpenTelemetry (OTLP) trace export |
| GAP-0029 | [#180](https://github.com/XDCIndia/xOneRS/issues/180) | P2 | cli | 5/10 | Interactive console: xone attach over IPC/HTTP |
| GAP-0035 | [#186](https://github.com/XDCIndia/xOneRS/issues/186) | P2 | observability | 7/10 | Metrics push: InfluxDB and Prometheus Pushgateway export |
| GAP-0036 | [#187](https://github.com/XDCIndia/xOneRS/issues/187) | P2 | tracing | 7/10 | Live chain-event hooks for in-process indexers (live tracer / execution extensions) |
| GAP-0030 | [#181](https://github.com/XDCIndia/xOneRS/issues/181) | P3 | tracing | 4/10 | Parity-style trace_* namespace (trace_block, trace_transaction, trace_filter, trace_replay*) |
| GAP-0031 | [#182](https://github.com/XDCIndia/xOneRS/issues/182) | P3 | keys | 4/10 | Account management CLI: xone account new / import / list / update |
| GAP-0032 | [#183](https://github.com/XDCIndia/xOneRS/issues/183) | P3 | rpc | 3/10 | Otterscan ots_* namespace |
| GAP-0033 | [#184](https://github.com/XDCIndia/xOneRS/issues/184) | P3 | observability | 3/10 | Shipped Grafana dashboard and alert rules for the Prometheus metrics |
| GAP-0034 | [#185](https://github.com/XDCIndia/xOneRS/issues/185) | P3 | storage | 3/10 | Snapshot import from an HTTPS URL with checksum, resume and pins |
| GAP-0037 | [#188](https://github.com/XDCIndia/xOneRS/issues/188) | P3 | docs | 4/10 | JSON-RPC reference documentation: every served method with parameters, result, errors and an example |

### Counts

| Priority | Gaps |
|---|---|
| P0 | 1 |
| P1 | 16 |
| P2 | 14 |
| P3 | 6 |

| Area | Gaps |
|---|---|
| rpc | 13 |
| tracing | 6 |
| observability | 5 |
| storage | 3 |
| cli | 3 |
| p2p | 3 |
| txpool | 1 |
| xdc-rpc | 1 |
| keys | 1 |
| docs | 1 |

Issues created: 37 (#152 to #188), all as `AnilChinchawale`, labels `gap-audit`, `area:<name>`, `priority:<P>`. Deduplication: `gh issue list --state all --search "<keywords> in:title"` per gap against the 78 existing issues; no existing issue covered a gap. Related issues are linked in each body (#14, #16, #36, #40, #41, #42, #121, #149 and others). One scope comment added to #176 (verify-state).

## Skipped

| ID | Item | Reason |
|---|---|---|
| SKIP-01 | Engine API (engine_*) and beacon-driven fork choice | No XDC counterpart: XDPoS selects the head itself; there is no consensus-layer client. --authrpc flags exist for join.sh compatibility only. |
| SKIP-02 | discv5 discovery | No XDC counterpart: geth-XDC and XDPoSChain peers speak discv4 only (XDC ping variant); a discv5 table would find no XDC peers. |
| SKIP-03 | DNS discovery (EIP-1459 enrtree) | No XDC counterpart: XDPoSChain params carry no enrtree URL and no XDC DNS tree is published. |
| SKIP-04 | eth/68+ wire protocol with ForkID | No XDC counterpart: the XDC network runs eth/100 and eth/101 without ForkID (#32). |
| SKIP-05 | Era/Era1 history files (import, export, download) | No XDC counterpart: no XDC era archives exist; xOne snapshot export/import (#38) covers history transfer between xOne nodes. |
| SKIP-06 | Blob pool, EIP-4844 sidecars, blob RPCs | No XDC counterpart: XDC has no blob transactions. |
| SKIP-07 | MEV builder APIs, Flashbots relay, sequencer journals | No XDC counterpart; XDPoS block production has no external builder market. |
| SKIP-08 | LES light server and Portal network | No XDC counterpart and no XDC light-client network; removed upstream. |
| SKIP-09 | miner_* runtime start/stop and personal_*/eth_sign/eth_sendTransaction (node-held keys) | Conflicts with architecture: ADR-010 signer safety and the Gate E mint protocol make runtime producer toggles and RPC-held keys unsafe; ADR-008 lists both as out. |
| SKIP-10 | XDCx / XDCxLending namespaces | Conflicts with accepted scope ADR-009 (#23): empty-tx routing only. |
| SKIP-11 | web3_sha3 | Parity rule: the pinned geth-XDC answers -32601 (archive vector web3_sha3.jsonl); serving it would break RPC parity. |
| SKIP-12 | Standalone RPC daemon over a shared datadir (Erigon rpcdaemon) | Conflicts with architecture: one node process owns the datadir exclusively (#97, ADR-005); a second reader process is a separate design decision. |
| SKIP-13 | Parlia / Bor-Heimdall consensus features (fast-finality votes, BLS vote keys, milestones) | Consensus-specific to BSC and Polygon; XDPoS V2 QC/TC finality already covers the role (#31, #35). |
| SKIP-14 | Stateless/zk proving, execution witnesses, L2 rollup stacks (ethrex L2, op-reth) | No XDC counterpart in the L1 client scope. |
| SKIP-15 | Ethstats reporting, health endpoints, static/trusted peers, remote signer, snapshot export/import, snap/1 serving | Not a gap: present in xOneRS (crates/xone-node/src/observe/ethstats.rs, metrics.rs /livez /readyz, enode.rs, crates/xone-signer/src/remote.rs, bin/xone/src/cmd/snapshot.rs, crates/xone-snap/src/server.rs). |
| SKIP-16 | Preimage export/import CLI (geth export-preimages, erigon export_preimages) | Covered: xOne moves preimages between nodes with xpre/1 and snapshot export (#38, crates/xone-snap/src/preimage.rs); no XDC operator flow needs a separate preimage file. |
| SKIP-17 | Offline state verification command (geth snapshot verify-state, reth db checksum) | Not split: added to the scope of the db CLI gap (GAP-0025, #176) by an issue comment, because it is the same shippable unit (rule: do not split gaps). |

## Expansion passes (Phase 5)

| Pass | Competitor expansion | Feature re-scan | New gaps |
|---|---|---|---|
| 1 | Initial discovery: 19 keyword queries, 6 topics, owner seeds, snowball from parents and forks | 60 feature probes over 10 repos plus targeted path checks | GAP-0001..GAP-0034 |
| 2 | Queries: otterscan, archive node, trace_filter, execution extension, XDC RPC node, XDPoS v2 client, HotStuff EVM: no new client scored 60+ | metrics push, plugins and live tracing, docs | GAP-0035, GAP-0036, GAP-0037 |
| 3 | No new candidates from the pass-2 keywords | importers (preimages), cron jobs (rejournal, prune), settings, verify-state | 0 (SKIP-16, SKIP-17 recorded) |

Stop condition met: pass 3 added zero competitors and zero gaps.

## Limits of this audit

- Competitor presence is verified by file path and identifier match (`verify` step of the generator); it is not a behavioural test. Cells marked *probable* in the matrix are keyword hits only.
- OURS was checked in product code (`crates/*/src`, `bin/xone/src`); tooling under `conformance/` and the Go oracles was excluded.
- Priorities follow the count rule with two stated exceptions (archive P0, XDC extensions P1). Several P1 RPC items reverse ADR-008 out-of-scope decisions; each issue says so, and the ADR change is part of the work.


## Second pass: bring advantages (2026-11-14)

Brief: for each strength of each execution-client family (geth, Nethermind, Besu, Erigon, Reth, ethrex, Nimbus, Zig projects), name the capability behind it with evidence, compare with xOneRS, and file one issue per strength that xOneRS lacks. Priority here is by impact for an XDC node operator, not by competitor count (rule recorded in `gap-ledger.json` as `priority_rule_advantages`): P1 = operators blocked or forced into a workaround today; P2 = large cost or capability gain for many operators; P3 = integrators, private networks or maintainers.

Method: competitor shallow clones of pass 1 (same commits); Zeam and Phant through the GitHub API (licence MIT, build files and release workflow read, not built); Nimbus resource claims from its README and nimbus.guide. xOneRS evidence from main `bd1e0c3`: ADR-005, `crates/xone-db`, `crates/xone-state`, `crates/xone-sync`, `crates/xone-net`, `crates/xone-rpc`, `docs/perf.md`, `docs/ops/`, `ci/release.sh`, `Dockerfile`, and the built release binary (`objdump -T`, `ldd`). Verdict per strength: [feature-matrix.md, Strengths](feature-matrix.md#strengths-second-pass-bring-advantages).

### Gaps and issues

| ID | Issue | Priority | Area | Strength of | Title |
|---|---|---|---|---|---|
| GAP-0038 | [#193](https://github.com/XDCIndia/xOneRS/issues/193) | P1 | release | Zig projects (Phant, Zeam) | Self-contained release binaries: static musl (or a pinned glibc floor), aarch64, size budget and a minimal image |
| GAP-0039 | [#194](https://github.com/XDCIndia/xOneRS/issues/194) | P2 | storage | Erigon (and Reth static files) | Compressed frozen segments: zstd for closed XSEG chunks of headers, bodies and receipts (and changesets once #135 moves them) |
| GAP-0040 | [#195](https://github.com/XDCIndia/xOneRS/issues/195) | P3 | storage | Erigon | Seeded history distribution: nodes serve their committed XSEG chunks with a hash manifest, new nodes fetch from several seeds and verify |
| GAP-0041 | [#196](https://github.com/XDCIndia/xOneRS/issues/196) | P2 | sync | Nethermind | History backfill after snap sync: download headers, bodies and receipts below the pivot in the background, to a configurable barrier |
| GAP-0042 | [#197](https://github.com/XDCIndia/xOneRS/issues/197) | P3 | perf | Nethermind (and ethrex) | RPC traffic recorder and open-loop replay benchmark (record real requests, replay them at a set rate, report latency per method) |
| GAP-0043 | [#198](https://github.com/XDCIndia/xOneRS/issues/198) | P3 | sdk | Reth | xOneRS as a library: a documented NodeBuilder with replaceable components (RPC modules, txpool, network handlers) and runnable examples |
| GAP-0044 | [#199](https://github.com/XDCIndia/xOneRS/issues/199) | P3 | p2p | Besu | Node permissioning for private XDPoS networks: enode allowlist file, runtime perm_* RPC, refuse unlisted peers in and out |
| GAP-0045 | [#200](https://github.com/XDCIndia/xOneRS/issues/200) | P2 | perf | Nimbus | Resource budgets as measured targets: RSS, CPU and disk ceilings per node type, a low-resource profile and a CI regression soak |
| GAP-0046 | [#201](https://github.com/XDCIndia/xOneRS/issues/201) | P3 | ci | ethrex | Code-size budget in CI: per-crate line count and dependency count on every PR, with a ceiling for the consensus crates |

Counts: P1 1, P2 3, P3 5; 9 issues (#193 to #201), labels `gap-audit`, `area:<name>`, `priority:<P>`; new area labels `area:release`, `area:sync`, `area:perf`, `area:sdk`, `area:ci`.

Deduplication: `gh issue list --state all --search` per strength (static binary, musl, aarch64, compress, zstd, seed, torrent, backfill, fast blocks, permission, allowlist, node builder, library, sdk, lines of code, resource/memory budget, rpc replay, load test, benchmark) against all issues; overlaps found and linked instead of filed: #187 (ExEx and plugin hooks), #185 (snapshot URL import), #16 and `crates/xone-node/src/disk.rs` (low-disk stop), #145 (disk-backed state, path-keyed trie), #135 (history in segments), #46 (closed; specified aarch64 but `ci/release.sh` still builds the host target only), #1 and #38 (no BitTorrent: GAP-0040 keeps HTTPS).

### Not filed

| Strength | Reason |
|---|---|
| geth path-based state, freezer, Erigon staged sync, Reth MDBX plus static files and modular crates, Besu Bonsai, Nethermind low-disk shutdown, Zig C-level speed and LMDB-style storage | Present in xOneRS (evidence in the strengths table) |
| Nethermind full pruning | Not applicable: path-keyed trie rows are overwritten in place and flat state keeps a 128-block window, so no dead hash-keyed nodes accumulate |
| Besu account permissioning, on-chain permissioning, private transactions | Not applicable: would change which transactions a producer accepts or which blocks are valid, so an XDC node would diverge from geth-XDC; permissioned XDC chains belong to XDC-Subnet |
| ethrex L2 orientation | Not applicable: SKIP-14 |
| Nimbus SQLite storage | Not applicable: MDBX kept by the #135 decision |
| Reth ExEx, Besu plugin API, Nethermind snapshot download | Filed in pass 1: #187, #185 |

### Limits

- Zeam is a consensus (lean) client and Phant a stateless research client; they are cited for build and packaging practice only, not for node features.
- The GLIBC_2.38 floor and the 19.4 MB size were measured on the local release build of `xOne/v0.1.0` on Ubuntu 24.04; the CI release build was not run.
- Size and memory targets in the issues are proposals until measured by the `docs/perf.md` procedure.


## Third pass: expansion from scratch (2026-11-14)

Brief: resume Phase 5 with the keywords of everything found since pass 1 (archive snapshots, seeded distribution, node permissioning, SDK and node builder, resource budgets, static binaries, CI budgets, RPC recorder, fast-sync history, tracing, metrics push, live hooks, XDC RPC extensions), the GitHub topics of the selected repos, awesome lists and web search; re-scan every selected competitor for missed areas; repeat until a pass adds nothing. Audited xOneRS: main `f4ae78d`. Competitor clones: `.gap-audit/refs/` (shallow, not committed); the first ten at the pass-1 commits, the four new ones at the commits in `gap-ledger.json`. The geth-XDC oracle pin `XDCIndia/go-ethereum@6fc8e8c2` (conformance/PIN) was read as the canonical reference for methods and flags; it is not scored as a competitor.

Priority rule (`priority_rule_pass3` in the ledger): operator impact first (the pass-2 scale), raised one level, never above P1, when 8+ of the 14 selected competitors have the capability or a canonical XDC client ships it in a namespace or default the fleet uses.

### Competitors

| Repo | Score | Overlap/Activity/Adoption/Access | Status | Reason |
|---|---|---|---|---|
| `BuildOnViction/victionchain` | 83 | 42/20/6/15 | selected | Viction (formerly TomoChain) client: Proof-of-Stake-Voting with masternodes, double validation, epoch checkpoints and randomize, the lineage XDPoS V1 was derived from. Not a fork of a selected repo; XDPoSChain descends from its code. |
| `harmony-one/harmony` | 73 | 26/20/12/15 | selected | EVM chain with FBFT (round-based BFT), staking epochs, rewards and an eth-compatible RPC; Go node with its own tracer API, Rosetta and webhooks. |
| `tempoxyz/tempo` | 73 | 26/20/12/15 | selected | Rust L1 on the reth SDK with its own BFT consensus layer, validator config, localnet, follow mode and snapshot download; adds operator surface over reth (unlike reth-bsc). |
| `kaiachain/kaia` | 70 | 32/20/3/15 | selected | Istanbul-BFT EVM chain with a validator committee, staking and rewards; adds chain-data export (Kafka, MySQL), homi network generator and live pruning over its geth base. |
| `BuildOnViction/vic-geth` | 78 | 40/20/3/15 | rejected | Score 78 but excluded by the fork rule: a rebuild of victionchain on current geth; its operator surface is geth's (selected) and its consensus is victionchain's (selected). |
| `etclabscore/core-geth` | 65 | 24/20/6/15 | rejected | Score 65 but excluded by the fork rule: geth derivative for Ethereum Classic; extra flags are PoW, ECBP-1100 and legacy light-client items; its trace_ module is already counted from Erigon and Nethermind. |
| `ethereum-optimism/op-geth` | 63 | 22/20/6/15 | rejected | Score 63 but excluded by the fork rule: geth fork whose additions are OP Stack L2 features (SKIP-14). |
| `celo-org/celo-blockchain` | 64 | 30/10/9/15 | rejected | Score 64 but end of life: the repository calls itself the previous Celo client, the last commit is a CI config change, and Celo now runs op-geth as an L2 (SKIP-14). |
| `Consensys-inc-archive/quorum` | 57 | 30/0/12/15 | rejected | Score 57: archived (GoQuorum end of life); its permissioning and QBFT live on in Besu (selected). |
| `berachain/bera-reth` | 58 | 20/20/3/15 | rejected | Score 58: reth SDK extension for a beacon-driven chain; adds nothing operator-facing over reth. |
| `FISCO-BCOS/FISCO-BCOS` | 57 | 10/20/12/15 | rejected | Score 57: consortium chain with its own RPC and P2P; not an Ethereum-family client (no devp2p, no eth JSON-RPC). |
| `shardeum/shardeum` | 58 | 8/20/15/15 | rejected | Score 58: sharded network whose validator is not an Ethereum-family node; eth RPC is a separate gateway. |
| `ava-labs/coreth` | 47 | 26/0/6/15 | rejected | Score 47: archived. |
| `ava-labs/subnet-evm` | 47 | 26/0/6/15 | rejected | Score 47: archived. |
| `klaytn/klaytn` | 53 | 32/0/6/15 | rejected | Score 53: archived; superseded by kaiachain/kaia (selected). |
| `XinFinOrg/XinFin-Node-Telemetry` | 28 | 5/20/3/0 | rejected | Score 28: log shipping helper for XinFin nodes, not a client; no licence. |

All four new selections have OSI licences (LGPL-3.0, Apache-2.0/MIT); no evidence is licence-restricted.

### Gaps and issues

| ID | Issue | Priority | Area | Seen in | Title |
|---|---|---|---|---|---|
| GAP-0047 | [#218](https://github.com/XDCIndia/xOneRS/issues/218) | P1 | sync | 5/14 + oracle | Required-block peer check with built-in network anchors (--eth.requiredblocks; Apothem reset block 83,665,617) |
| GAP-0048 | [#220](https://github.com/XDCIndia/xOneRS/issues/220) | P1 | tracing | 9/14 + oracle | Divergence tracing: debug_intermediateRoots, debug_traceBadBlock, debug_traceBlock (raw RLP), debug_standardTraceBlockToFile and debug_traceChain |
| GAP-0049 | [#221](https://github.com/XDCIndia/xOneRS/issues/221) | P1 | p2p | 9/14 + oracle | Persistent peer table: remember good peers across restarts (geth nodes DB, reth peers file) |
| GAP-0050 | [#222](https://github.com/XDCIndia/xOneRS/issues/222) | P2 | rpc | 8/14 + oracle | Node self-description RPC: eth_config (EIP-7910 fork configuration) and eth_capabilities (served history ranges) |
| GAP-0051 | [#223](https://github.com/XDCIndia/xOneRS/issues/223) | P2 | cli | 6/14 | Private XDPoS network generator: genesis with masternodes and system contracts, node keys and per-node configs (xone network init) |
| GAP-0052 | [#224](https://github.com/XDCIndia/xOneRS/issues/224) | P2 | sync | 4/14 + oracle | Sync stop targets: --exitwhensynced and --synctarget (stop at a given block hash) |
| GAP-0053 | [#225](https://github.com/XDCIndia/xOneRS/issues/225) | P2 | storage | 4/14 + oracle | Separate directory for frozen history segments (--datadir.ancient / --datadir.static-files) so history can sit on cheaper disk |
| GAP-0054 | [#226](https://github.com/XDCIndia/xOneRS/issues/226) | P2 | observability | 5/14 + oracle | Slow-block log: one structured line with per-phase timings for each block slower than a threshold (--debug.logslowblock) |
| GAP-0055 | [#227](https://github.com/XDCIndia/xOneRS/issues/227) | P2 | cli | 7/14 + oracle | Environment-variable configuration: every flag settable as XONE_<FLAG> (container and systemd deployments) |
| GAP-0056 | [#228](https://github.com/XDCIndia/xOneRS/issues/228) | P2 | release | 8/14 | OS packages: .deb and .rpm with the systemd unit, and a signed APT/YUM repository |
| GAP-0057 | [#230](https://github.com/XDCIndia/xOneRS/issues/230) | P2 | cli | 9/14 + oracle | Standalone EVM tool: t8n (transition), state-test and block-test runners on the xOneRS executor (xone evm) |
| GAP-0058 | [#231](https://github.com/XDCIndia/xOneRS/issues/231) | P3 | xdc-rpc | 0/14 + oracle | xdc_ RPC namespace served by geth-XDC: the eth block-chain methods under xdc_ plus xdc_getStateHealStatus |
| GAP-0059 | [#232](https://github.com/XDCIndia/xOneRS/issues/232) | P3 | sdk | 1/14 | Chain-data exporter: stream blocks, receipts, logs and traces to Kafka (reorg-aware, resumable), for explorers and indexers |

Counts: 13 issues (#218 to #232); P1 3, P2 8, P3 2; by area: cli 3, observability 1, p2p 1, release 1, rpc 1, sdk 1, storage 1, sync 2, tracing 1, xdc-rpc 1. Labels `gap-audit`, `area:<name>`, `priority:<P>` (all labels existed).

Scope added to existing issues by comment instead of new issues (rule: do not split gaps): #168 (five more XDC methods), #160 (eight more debug diagnostics), #156 (transactionReceipts subscription), #163 (start-up `--log.vmodule`), #171 (six more RPC listener and safety flags).

Deduplication: every candidate was searched with `gh issue list --state all --search` (Apothem reset, required block, stale branch, slow block, cold storage, ancient, localnet, genesis generator, evm tool, state test, packages, eth_config, capabilities, xdc_ namespace, heal status, rpc metrics, per-method, peers persist, nodes database, kafka, exitwhensynced) and against the bodies of GAP-0001 to GAP-0046.

### Method

- RPC: the method sets of XDPoSChain and of the oracle pin were extracted from their registered API types (`internal/ethapi`, `eth`, `eth/filters`, `eth/tracers`, `node`, `consensus/XDPoS`) and diffed against the methods `crates/xone-rpc/src` registers and the 46 existing gap bodies; 137 oracle methods are not served, all but the ones above are covered by an existing gap or a skip.
- Flags: the 274 flag names of the oracle pin were diffed against `docs/cli/flags.md` and `bin/xone/src/flags.rs` and the existing bodies.
- Probes over all 14 clones: RPC authentication and TLS, divergence tracing, required blocks, node database, network generators, stop targets, cold-storage paths, slow-block logs, environment configuration, OS packages, t8n tools, exporters, webhooks, version checks, per-method RPC metrics.

### Not filed (pass 3)

| ID | Item | Reason |
|---|---|---|
| SKIP-18 | JSON-RPC authentication on the public endpoints (Besu credentials/JWT, reth --rpc.jwtsecret, Harmony auth port) | Covered: #60 defines authentication enforcement per transport; the mechanism belongs there. |
| SKIP-19 | TLS on the RPC listener (Besu --rpc-http-tls-enabled) | 1 of 14; deployments terminate TLS at a reverse proxy, and #60 owns transport exposure. |
| SKIP-20 | Per-method RPC latency metrics (geth rpc/metrics.go, reth rpc-builder metrics.rs, Erigon, Besu, Nethermind, Kaia, XDPoSChain) | Covered: GAP-0033 (#184) step 4 adds the per-method RPC latency histogram. |
| SKIP-21 | Start-up per-module log filter (--log.vmodule) | Covered: added to #163 (GAP-0012) by comment; the filter exists in crates/xone-node/src/observe/logs.rs but no flag fills it. |
| SKIP-22 | eth_getOwnerByCoinbase, eth_getTokenStats, XDPoS_getBlockInfoByV2EpochNum, XDPoS_calculateBlockInfoByV1EpochNum, XDPoS_getV2BlockByHeader | Covered: added to #168 (GAP-0017) by comment (do not split gaps). |
| SKIP-23 | debug_getBlockRlp, debug_printBlock, debug_preimage, debug_dbGet, debug_dbAncient(s), debug_discoveryV4Table, debug_clearTxpool | Covered: added to #160 (GAP-0009) by comment. |
| SKIP-24 | eth_subscribe("transactionReceipts") | Covered: added to #156 (GAP-0005) by comment. |
| SKIP-25 | RPC listener and safety flags: --rpc.allow-unprotected-txs, --rpc.http-body-limit, --rpc.logquerylimit, --rpc.rangelimit, --http.rpcprefix, --ws.rpcprefix | Covered: added to #171 (GAP-0020) by comment. |
| SKIP-26 | XDPoS_propose, XDPoS_discard, XDPoS_proposals (clique-style signer votes) | ADR-008 working scope lists XDPoSAdmin_* as out; the oracle serves propose and discard only on the authenticated endpoint. |
| SKIP-27 | Rosetta (Mesh) API | 1 of 14 (Harmony); a second API surface outside the JSON-RPC parity scope of #42. |
| SKIP-28 | Event webhooks (Harmony webhooks, Nethermind health-check webhooks) | Covered: the SkyNet heartbeat pushes node status to a configured URL (crates/xone-node/src/observe/heartbeat.rs) and GAP-0033 (#184) adds alert rules. |
| SKIP-29 | Version and vulnerability check (geth-derived version-check) | 0 of 14 selected (only core-geth and vic-geth, both rejected); no XDC advisory feed exists to check against. |
| SKIP-30 | Bootnodes fetched from an HTTP endpoint (Tempo --tempo.bootnodes-endpoint) | 1 of 14; bootnode lists ship with the release (ops/bootnodes). |
| SKIP-31 | Live state pruning, database migration, offline state GC (Kaia db_migration, Viction cmd/gc) | Not applicable: path-keyed trie rows are overwritten in place (second pass) and the storage engine is the #135 decision. |
| SKIP-32 | Chain-specific features: Kaia governance_, mainbridge/subbridge, auction; Harmony staking, sharding, multi-BLS keys; Tempo zones and DKG; Viction TomoX | No XDC counterpart: XDC subnets belong to XDC-Subnet, XDCx is ADR-009 empty-tx routing only, XDPoS has one key per masternode (SKIP-13 analogue). |
| SKIP-33 | geth-XDC flags with no XDC counterpart or already covered: beacon.*, blobpool.*, crypto.kzg, era and history import/export, bintrie, override.bpo*, miner.*, cache.*, gogc, memorylimit, fdlimit, store-reward, fastsyncpivot*, nodekeyhex, maxpendpeers | SKIP-01, -05, -06, -09; Go runtime tuning (xOne has --cache and ADR-007 limits); rewards are computed, not filed (GAP-0017); pivots are pinned by --snap.pins (#14); the node key is a datadir file (GAP-0027); peer budgets are #16; dumpgenesis is in GAP-0051. |
| SKIP-34 | Historical re-execution under candidate rules (Tempo shadow replay) | Covered: the conformance harness replays on xOne's own state (#10, conformance/harness). |

### Expansion passes (third run)

| Pass | Competitor expansion | Feature re-scan | New gaps |
|---|---|---|---|
| 3a | 18 keyword queries, 12 topics plus the selected repos' own topics, awesome-reth, web search, XinFinOrg and BuildOnViction org listings: 16 candidates scored, 4 selected (victionchain 81, harmony 73, tempo 73, kaia 70) | oracle and XDPoSChain RPC and flag diffs; 17 probes over 14 clones | GAP-0047 to GAP-0059 (13), 5 scope comments, SKIP-18 to SKIP-34 |
| 3b | Keywords of the 3a gaps (puppeth, t8n, chaindatafetcher, evm localnet, required blocks) and the new repos' topics (pbft, proof-of-stake, harmony-protocol, sharding): FISCO-BCOS 57, shardeum 58, no new candidate at 60+ | Kaia subcommands and 18 RPC namespaces, Harmony namespaces, Tempo CLI modules, Viction flags against XDPoSChain: every item is a filed gap, a scope comment or a skip | 0 |

Stop condition met: pass 3b added zero competitors and zero gaps.

### Limits

- Presence is file and identifier evidence, not a behavioural test; XDPoSChain `build/deb` holds only shell completions and was not counted as packaging.
- The oracle pin is private; its links resolve only for readers with access.
- Kaia, Harmony and Tempo are chains of their own; only their operator features were compared, not their consensus.
