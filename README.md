# xOne

XDC execution-client rebuild in Rust. The current rebuild proposal is tracked in [issue #1](https://github.com/XDCIndia/xOne/issues/1).

## Status

Only genesis and chain-spec code exist so far. There is no sync, EVM, consensus, networking or RPC yet, and `xone` cannot run a node.

| Crate | What it has |
| --- | --- |
| `crates/xone-primitives` | XDC block `Header` and its RLP encoding and hashes |
| `crates/xone-chainspec` | Chain specs for mainnet (50), apothem (51), devnet (5551) and devnet551 (551), and genesis loading |
| `bin/xone` | `--version` stamp and `genesis-hash` |

The genesis hashes are tested against the constants in geth-XDC `params/config.go`. The pinned geth-XDC commit is in `crates/xone-chainspec/genesis/PIN`.

## Build and run

The toolchain is pinned in `rust-toolchain.toml` (rustup installs it on first use).

```sh
cargo build --release
cargo test
./target/release/xone --version
./target/release/xone genesis-hash --network mainnet   # mainnet | apothem | devnet | devnet551
```

## Architecture and audit planning

- [Architecture, scalability and post-quantum readiness review](docs/reviews/2026-11-10-architecture-scale-pq-review.md)
- [Claude audit and dedicated issue-creation prompt](docs/prompts/architecture-audit-and-issue-creation.md)
- [Documentation guide and proposed workflow](docs/README.md)

The documents contain recommendations and qualification gates. The Claude prompt is used only when explicitly invoked; adding these files does not start an audit, create finding issues or operate validators.
