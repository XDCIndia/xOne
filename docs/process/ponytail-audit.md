# Ponytail whole-repo audit of xOneRS

Method: the whole-repo audit of [ponytail](https://github.com/dietrichgebert/ponytail) (`skills/ponytail-audit/SKILL.md` at `96608e0`), run by hand, lean part only (delete, reuse, std or native, yagni, merge). Tree: `origin/main` at `bd1e0c3`, 2026-11-14. Every finding was checked with a grep of the whole tree, tests, conformance and scripts included.

**What this repo does:** xOneRS is a Rust execution client for the XDC network (XDPoS V1 and V2). It follows, syncs, serves RPC and produces blocks, and it is checked block by block against geth-XDC. Assumed load: a validator or RPC node at chain tip on mainnet, Apothem and net2040, plus CI and developer builds.

Rules for this audit: nothing is proposed that a consensus rule, the gate DAG (#5, #57) or an accepted interface item needs. Those items are listed under "Kept on purpose" with the reason.

## Should fix

1. **Finished spikes and the PQ lab build in every test run** (`Cargo.toml:3`, `spikes/wide-balance`, `spikes/net`, `labs/pq`)
   - **What this is:** three experiment crates (2,372 lines) are workspace members. CI runs `cargo clippy --all-targets` and `cargo test --locked` on all members (`.github/workflows/ci.yml:25-26`).
   - **Problem:** the spikes are done (`docs/adr/ADR-002.md` records the results). They still compile and test on every push, and `pq-lab` adds 19 crates that no product crate uses (`cargo tree -p pq-lab` against all other members).
   - **Fix:** add `default-members` to `Cargo.toml` with every member except `spikes/wide-balance`, `spikes/net` and `labs/pq`. Keep the directories: `spikes/wide-balance/vectors/matrix.json` is read by `crates/xone-evm/tests/wide_matrix.rs:119`. Run a spike by `-p` when needed.
   - **If we skip it:** every CI and local test run pays for code that answers a closed question.

2. **Five storage traits with no implementation** (`crates/xone-db/src/lib.rs:68-100`)
   - **What this is:** `HeaderProvider`, `BodyProvider`, `ReceiptProvider`, `StateProvider` and `BlockWriter`, the M0 storage contract.
   - **Problem:** no type implements any of them anywhere in the tree. The real store is `crates/xone-db/src/mdbx.rs` and `crates/xone-sync/src/store.rs` (`Store`). The only other mention is a doc comment (`crates/xone-node/src/services/producer.rs:1163`). The name `StateProvider` also clashes with the real struct `xone_state::StateProvider` (`crates/xone-state/src/provider.rs:92`).
   - **Fix:** delete the five traits (34 lines) and point the doc comment at `xone_db::Durability`.
   - **If we skip it:** readers take a dead contract for the write path.

3. **The snap test kit ships in the library** (`crates/xone-snap/src/lib.rs:18`, `crates/xone-snap/src/testkit.rs`)
   - **What this is:** `pub mod testkit` (a loopback peer and flat-state helpers, 110 lines) is compiled into every build.
   - **Problem:** only `crates/xone-snap/tests` and `conformance/snap/tests/plan.rs:13` use it. `xone-db` already solves the same case with an opt-in feature (`crates/xone-db/Cargo.toml:15-17`, `fault-injection`).
   - **Fix:** reuse that pattern: a `testkit` feature in `crates/xone-snap/Cargo.toml`, `#[cfg(feature = "testkit")]` on the module, and the feature on the dev-dependency of the two users.
   - **If we skip it:** test helpers stay in the shipped binary graph.

## Nice to have

4. **`same_set` is written twice in consensus** (`crates/xone-consensus/src/penalty/mod.rs:12`, `crates/xone-consensus/src/v1/checkpoint.rs:40`)
   - **What this is:** two copies of "sort both lists, compare". One uses `sort_unstable`, one `sort`; for `Address` the result is the same.
   - **Problem:** two copies of a rule that must stay the same.
   - **Fix:** keep `penalty::same_set`, delete the copy in `v1/checkpoint.rs`, import the kept one. The consensus vectors (`crates/xone-consensus/tests/`) must stay green.
   - **If we skip it:** a later fix lands in one copy only.

5. **Clock and peer-id helpers are copied in `xone-node` and `bin/xone`** (`crates/xone-node/src/services/core.rs:266`, `crates/xone-node/src/services/snapsync.rs:145,149`, `crates/xone-node/src/services/chain.rs:752`, `bin/xone/src/cmd/mod.rs:66`)
   - **What this is:** `now()`, `now_secs()` and `short(&PeerId)` exist in two or three places with the same body.
   - **Problem:** duplicates. `core::now` is already `pub`.
   - **Fix:** use `crate::services::core::now` in `snapsync.rs` and `xone_node::services::core::now` in `bin/xone`; keep one `short` in `services/mod.rs`.
   - **If we skip it:** small drift risk, about 20 extra lines.

6. **Two direct dependencies are not needed in `[dependencies]`** (`crates/xone-rpc/Cargo.toml:21`, `crates/xone-chainspec/Cargo.toml:14`)
   - **What this is:** `serde` in `xone-rpc` and `alloy-rlp` in `xone-chainspec`.
   - **Problem:** `xone-rpc/src` uses only `serde_json`; no `serde::` path or derive. `xone-chainspec/src` never uses `alloy_rlp`; only `tests/custom_chain_id.rs:4` does.
   - **Fix:** delete `serde` from `xone-rpc`; move `alloy-rlp` to `[dev-dependencies]` of `xone-chainspec`.
   - **If we skip it:** two false edges in the dependency graph.

## Not filed (covered elsewhere or not shippable now)

- **Snap pre-pivot completeness is not wired.** `finish`, `History`, `Need` and `Incomplete` (`crates/xone-snap/src/client.rs:978-1066`, about 100 lines) are used only by `crates/xone-snap/tests/sync.rs` and `conformance/snap`. This is the acceptance of #37: wire it there, do not delete it.
- **Two copies of libsecp256k1.** `secp256k1` 0.30 comes from the `reth-ecies` pin (`crates/xone-net/Cargo.toml:19-21`) and 0.31 from `alloy-primitives` 1.7. Both are upstream pins. Revisit when the reth pin moves (`docs/adr/ADR-002.md` decision item 4).

## Kept on purpose

| Item | Why it stays |
|---|---|
| `Verifier`, `Secp256k1Verifier`, one implementation (`crates/xone-primitives/src/crypto.rs:151`) | The #17 acceptance asks for an interface with an algorithm id and domain tag (`docs/pq/README.md:25`). |
| `RemoteSigner` and `Fenced<R>`, one test implementation (`crates/xone-signer/src/remote.rs:36`, `crates/xone-consensus/src/unit.rs:91`) | #40 and `docs/adr/ADR-010-signer-safety.md` require the fenced remote-signer interface. |
| `WideBalance` on `num-bigint` (`crates/xone-primitives/src/wide_balance.rs:30`) | Consensus: geth-XDC balances can pass 2^256 (`spikes/wide-balance/REPORT.md`); `U256` cannot hold them. |
| `signal-hook` (`crates/xone-node/src/lifecycle.rs:59`) | `tokio::signal` needs the runtime in the lifecycle thread and pulls `signal-hook-registry` anyway. No net cut. |
| `libmdbx` FFI (`Cargo.toml`, `crates/xone-db/src/mdbx.rs`) | `docs/adr/ADR-005-storage-and-publication.md`; the one audited unsafe crate. |
| `xone_sync::testkit::MemStore` (`crates/xone-sync/src/testkit/mem.rs`) | The offline commands of `bin/xone` use it (`bin/xone/src/cmd/mod.rs:25`). |
| `xone_db::fault` (`crates/xone-db/src/fault.rs`) | Already behind the `fault-injection` feature. |

Verdict: healthy at the size of the product. Fix 1 and 2 first; the rest are small.

Lean: about -2,372 lines from the default build, about -60 lines of source, -1 direct dependency (and 1 moved to dev), -19 crates from the CI test graph.

Not checked: build time (not measured), the split of the seven src files over 1,000 lines (`crates/xone-sync/src/mdbx/mod.rs` and others), and the correctness parts of the ponytail audit (bugs, security, scale), which the gap audit (#152-#188) and the review issues cover.

## Issues

| # | Issue |
|---|---|
| 1 | #206 |
| 2 | #207 |
| 3 | #208 |
| 4 | #209 |
| 5 | #210 |
| 6 | #211 |
