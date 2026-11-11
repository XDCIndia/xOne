# Spike XA-03 / S2: reth RLPx crates against the XDC wire (eth/100, eth/101, xdpos2)

Issue XDCIndia/xOne#8. Plan risk K6 and change C2. Oracle peer: geth-XDC at `6fc8e8c2088b7f18264f5663036669d04fa7becf`
(`oracle/PIN`), built from that tree, run on 127.0.0.1 only. Paths of the form `file:line` are in that tree unless they start
with `reth` or `crates/` (reth tag named in the text).

## Verdict

1. **No published reth network crate exists.** `reth-ecies`, `reth-eth-wire`, `reth-eth-wire-types`, `reth-network`,
   `reth-network-peers` are `0.0.0` placeholders on crates.io (all created 2026-01-23; checked with the crates.io API on
   2026-11-11). The pin is a git revision. `reth-primitives-traits` and `reth-codecs` are real crates (`0.8.2`, from `reth-core`).
2. **Stock `reth-eth-wire` and `reth-network` cannot speak eth/100 or eth/101 without a fork.** The blocking types are
   named below. There is also no `xdpos2` capability to add: at the pin, `xdpos2` is eth version 100 and 101 under the name `eth`.
3. **The K6 fallback works with no fork and no patch.** `reth-ecies` + `reth-network-peers`, pinned by git revision, plus a
   230-line p2p/eth-wire module of our own (`src/lib.rs`, with doc comments, constants and the error type), completed against the local geth-XDC node: RLPx handshake,
   Hello, eth/101 and eth/100 negotiation, XDC Status with no fork id, a `GetBlockHeaders`/`BlockHeaders` round trip for
   block 0 whose hash equals the genesis hash of `xone-chainspec`, and an XDPoS V2 `Vote` that geth decoded and processed.
4. **Cost.** The session layer, the capability matcher, Ping/Pong, snappy and the eth message set are ours. Discovery,
   peer manager and snap/1 are not in this spike (see "Not covered"). Add them to the M5 estimate as plan change C2 says
   for choice (b).

## What was run, and the result

Local node (`run-local-geth.sh`; `GOTOOLCHAIN=local`, Go 1.25.2, `go build ./cmd/geth` from the pinned tree, binary kept
outside the repo; geth reports `XDC/v1.17.10-xdc.21`):

```
geth --datadir D --xdcdevnet --nodiscover --bootnodes "" --nat extip:127.0.0.1 --netrestrict 127.0.0.0/8 \
     --port 30399 --maxpeers 4 --ipcpath D/geth.ipc --authrpc.addr 127.0.0.1 --authrpc.port 31399 --verbosity 5
```

`--xdcdevnet` is chain and network id 5551 with the embedded devnet genesis (`cmd/utils/flags.go:2275-2278`,
`core/genesis_xdc.go:84`). It is not devnet551: `core/genesis_xdc.go:96` builds chain 551 but no flag selects it, and
`crates/xone-chainspec/genesis/devnet551.json` has no `config` block. The genesis of 5551 is the `xone-chainspec`
`devnet` genesis, hash `0xb8be003946a9c2688e9f1e255a5567c3d144293ccde2ffb38452a5840081b402`; the test asserts both
the peer's Status and the decoded block 0 header equal the hash that `xone-chainspec` computes. No discovery, no
bootnode, no key. `ss -tnp` showed no outgoing connection from the node.

Handshake and wire test (`tests/local_geth.rs`, `#[ignore]` by default):

```
$ GETH_BIN=<geth-xdc> DATADIR=<scratch> spikes/net/run-local-geth.sh &        # prints the enode, writes DATADIR/enode.txt
$ XONE_GETH_ENODE=$(cat DATADIR/enode.txt) XONE_GETH_LOG=DATADIR/geth.log \
    cargo test --locked -p net-spike --test local_geth -- --ignored --nocapture --test-threads=1
running 2 tests
test eth_100_bare_framing ... ok
test eth_101_status_headers_and_consensus_messages ... peer hello: XDC/v1.17.10-xdc.21/linux-amd64/go1.25.2 caps [eth/63, eth/69, eth/70, eth/100, eth/101, snap/1]
vote bytes sent: 106
geth: TRACE[11-11|08:35:27.824] Receive Vote  hash=0x3bb2ce19..8c661c "voted block hash"=0x1111..1111 number=1 round=1
ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.34s
```

Offline byte-exact test (`tests/wire.rs`, runs in `cargo test`): the RLP of `Status62`, `GetBlockHeaders` (bare and with
request id), `Vote`, `Timeout` and `SyncInfo`, encoded by Rust, equals `vectors.json`, which `oracle/run.sh` prints from
the pinned geth types (`eth.StatusPacket62`, `eth.GetBlockHeadersRequest`, `eth.GetBlockHeadersPacket`, `types.Vote`,
`types.Timeout`, `types.SyncInfo`). Decoding the geth bytes returns the same value. A six-element Status (a fork id) is
rejected.

| Item | Result | Evidence |
| --- | --- | --- |
| RLPx (ECIES) handshake and Hello | works with `reth-ecies` `ECIESStream::connect` | test passes |
| Capabilities of the local node | `eth/63, eth/69, eth/70, eth/100, eth/101, snap/1`; no capability named `xdpos2` | Hello printed by the test; `eth/protocols/eth/protocol.go:80` (`ProtocolVersions = {XDPOS2RID, XDPOS2, ETH70, ETH69, ETH63}`), `:50` (`ProtocolName = "eth"`), `eth/backend.go:868-876` (snap) |
| eth/101 negotiated when both are offered | yes (highest shared version) | `p2p/peer.go:438-459` (`matchProtocols`) |
| eth/100 negotiated when only it is offered | yes | `eth_100_bare_framing` passes; geth log `XDPOS2 GetBlockHeaders RECEIVED (serving)` is the bare-request handler (`eth/protocols/eth/handlers.go:41-54`) |
| Message id space | eth/100 and eth/101 have 229 messages; eth offset is 16; Vote is wire id `16 + 0xe0` | `protocol.go:89`, `p2p/peer.go:43`, `protocol.go:125-127` |
| Status | five fields `[version, network id, td, head, genesis]`, **no fork id**, for 100 and 101. geth checks network id, version and genesis only | `handshake.go:110-117` (100 and 101 take `handshake62`), `:149-176`, `:178-190`, `protocol.go:196-202` |
| Fork id (eth/69, eth/70 only) | sent and filtered by the modern `StatusPacket`; not used by XDC peers | `handshake.go:118-147` |
| Header round trip, eth/101 | request `[id, [origin, amount, skip, reverse]]`, reply `[id, [headers]]` | `protocol.go:234-237`, `peer.go:676-690` (`version == XDPOS2` is the only bare case), `handlers.go:55-62`; test decodes the reply and hashes block 0 |
| Header round trip, eth/100 | request `[origin, amount, skip, reverse]`, reply is the bare list `[headers]` | `handlers.go:41-54`, `peer.go:450-452` (`ReplyBlockHeadersRLPLegacy`) |
| Block 0 hash | the decoded header hashes to the `xone-chainspec` devnet genesis hash (xone-primitives `Header::hash`) | test asserts equality for both versions |
| xdpos2 consensus messages | `Vote`, `Timeout`, `SyncInfo` are codes `0xe0`, `0xe1`, `0xe2` in the eth/100 and eth/101 code space, body is the bare RLP of the type | `protocol.go:125-127`, `handlers.go:1158-1190`, `eth/handler_eth.go:144-185`; Rust encodes equal geth (`wire.rs`) |
| Vote delivered | geth decoded our `Vote` into `types.Vote` (hash, round, number match) and passed it to the engine; verification fails on a fresh node (`no checkpoint header at 900`), the peer stays connected and answers the next header request | `eth/bft/handler.go:191`, geth log, second header round trip in the test |
| Timeout and SyncInfo | byte-exact against geth types only; not sent on the wire | `wire.rs` |
| Legacy peer | not run: no legacy XDPoSChain binary is allowed or available. The eth/100-only session exercises geth-XDC's XDPOS2 (100) handlers, the shape legacy peers speak (`protocol.go:55-61`, 100 stays "so legacy fleet nodes remain peers") | stated limit |

## What needed a workaround

The workaround for every blocker below is the same: do not use `reth-eth-wire`'s `P2PStream`, `EthStream` or
`reth-network`. Use `reth-ecies` for the encrypted frames and write the p2p layer. It is `spikes/net/src/lib.rs`: the Hello and capability types (`alloy-rlp` derive), `Session` (Hello, snappy via the `snap` crate,
Ping/Pong, Disconnect, eth offset) and the three eth message types the test needs (`Status62`, `GetHeaders`, `GetHeadersRid`).

Blocking types and functions (reth `v2.8.1`, `d763342f`; same code at `v2.3.0` where noted; proven by
`spikes/net/probe`, which runs on `+nightly` because `v2.8.1` needs rustc 1.95):

```
$ cargo +nightly test --manifest-path spikes/net/probe/Cargo.toml -- --nocapture
try_new(eth/100) = Some(ParseSharedCapability(UnsupportedVersion(ParseVersionError("100"))))
test eth_100_cannot_be_negotiated ... ok
```

| Blocker | Location (v2.8.1) | Effect |
| --- | --- | --- |
| `EthVersion` is a closed `#[repr(u8)]` enum, 66 to 72 (66 to 69 at v2.3.0); `TryFrom<u8>`, `TryFrom<&str>` reject 100 and 101 | `crates/net/eth-wire-types/src/version.rs:21-46`, `:131-174` | No type for an XDC version. |
| `SharedCapability::new` maps name `eth` to `EthVersion::try_from(version)?`; any `eth/100` fails with `UnsupportedVersion` | `crates/net/eth-wire/src/capability.rs:54-72`; called from `shared_capability_offsets` `:297-370` (identical at v2.3.0, `diff` of lines 50-75 is empty) | Capability negotiation fails: the peer is dropped at Hello. `SharedCapabilities` is a tuple struct with a private field, so it cannot be built by hand. |
| `UnauthedP2PStream::handshake` calls `SharedCapabilities::try_new` | `crates/net/eth-wire/src/p2pstream.rs:178-179` | Same failure. `P2PStream::new` needs a `SharedCapabilities`. |
| `UnifiedStatus` has a mandatory `forkid: ForkId` and a typed `EthVersion` | `crates/net/eth-wire-types/src/status.rs:15-32` | A status without a fork id has no type. The trait `EthRlpxHandshake` (`crates/net/eth-wire/src/handshake.rs:20-30`) lets a node replace the status exchange, but it runs after capability negotiation, so it cannot help. |
| Message sets are `EthMessage` per `EthVersion`, all with request ids (eth/66+) | `crates/net/eth-wire-types/src/message.rs`, `crates/net/network/src/session/active.rs:537` | eth/100 bare requests and the 229-message code space have no type. |
| `reth-network` sessions use the above | `crates/net/network/src/session/mod.rs:29`, `:495` (`UnauthedP2PStream::new(stream)`) | The whole session and peer manager inherit the limit. |
| `snap` is `SnapVersion::V2` only | `crates/net/eth-wire-types/src/snap.rs:19-23` | No snap/1. The local geth-XDC advertises `snap/1`. |

An extension that exists and does not help: `ProtocolHandler` / `RlpxSubProtocols`
(`crates/net/network/src/protocol.rs:20-45`, `:120`; `add_rlpx_sub_protocol` `crates/net/network/src/config.rs:556`) adds a
**separate** capability. eth/100 is not separate: it is the `eth` capability itself. A separate capability named
`xdpos2` would not be negotiated by any XDC peer.

The XDC fork of reth confirms the route. `reth-apothem-spec/crates/net/xdc-wire` (3,868 lines with its design document)
is an own stream and handshake over `reth-eth-wire`'s `Capability` type, with this note at `src/capability.rs:26-32`:
"eth/100 IS the XDPoS2 consensus protocol ... The old xdpos/100 cap was never negotiated by the XDC fleet".

## The `xdpos2` premise, corrected

The plan and the issue speak of an `xdpos2` subprotocol beside `eth`. At the pin it is not a separate protocol:

* `XDPOS2 = 100` and `XDPOS2RID = 101` are versions of the protocol named `eth` (`protocol.go:39-40`, `:50`).
* The consensus messages are `VoteMsg`, `TimeoutMsg`, `SyncInfoMsg` = `0xe0`, `0xe1`, `0xe2` in the same code space
  (`protocol.go:125-127`); the code space has 229 entries (`:89`), the eth/69 space has 18.
* The word "xdpos2" appears as a name for versions 100/101 (`eth/protocols/eth/discovery.go:72`, `protocol.go:69`).
  A peer that is `xdpos2/100` in that sense still advertises `eth/100`.
* One more reason the capability name matters for monitoring: a peer that advertises only eth/100 and eth/101 is an
  `eth` peer whose `EthVersion` cannot be parsed by stock reth. This is the "invisible to peer counts" case of the fleet notes.

If a deployed legacy binary advertises a capability literally named `xdpos2`, this spike did not see one. Only the local
geth-XDC was spoken to. The negotiation code compares `(name, version)` pairs, so adding `Cap { name: "xdpos2", version: 100 }`
to the offered list is a one-line change; it has not been exercised.

## `cargo tree` at the exact rev

Pin used by the workspace spike: reth tag `v2.3.0`, `9384bc53d8c0c77e59cac83fdaaf3b372c6d2216`, crates `reth-ecies` and
`reth-network-peers` only. Reason: reth's `rust-version` is 1.93 at `v2.3.0`, 1.95 at `v2.4.0` to `v2.8.1`, 1.96 at
`v2.10.1` (raw `Cargo.toml` of each tag), and the workspace toolchain is `1.94.0` (`rust-toolchain.toml`). `cargo check` at
`v2.8.1` with `1.94.0` stops with: `reth-ecies@2.8.1 requires rustc 1.95`, `reth-network-peers@2.8.1 requires rustc 1.95`,
`reth-primitives-traits@0.8.2 requires rustc 1.94.1`. `ecies` and `peers` changed little since: `v2.3.0` to `v2.10.1`
is +59/-37 lines in `ecies/src` and +155/-76 in `peers/src`, and `v2.8.1` to `v2.10.1` is +0/-0 and +1/-1, with the same API used here.

Files in `trees/`:

| File | Content |
| --- | --- |
| `net-spike-v2.3.0.txt` | full `cargo tree --locked -p net-spike -e normal`: 136 packages, no `revm`, no `evm2`, no `reth-chainspec` |
| `ecies-peers-v2.8.1.txt`, `ecies-peers-v2.10.1.txt` | `reth-ecies` and `reth-network-peers` only, at `d763342f` and `58ebce14`: 129 packages each, no `revm`, no `evm2` |
| `lower-crates-v2.8.1.txt`, `.unique.txt` | `reth-ecies`, `reth-eth-wire`, `reth-eth-wire-types`, `reth-network-peers` at `d763342f`: 264 packages, includes `revm 43.0.3` and `alloy-evm 0.39.0` (through `reth-chainspec` and `reth-primitives-traits`) |
| `lower-crates-v2.10.1.txt`, `.unique.txt` | same four at `58ebce14ae63dab040beae301e1578cd0dc55e8a`: 213 packages, three `revm-*` leaf crates (`revm-primitives 43.0.0`, `revm-bytecode 43.0.0`, `revm-state 43.0.1`), no `evm2` |
| `reth-network-v2.8.1.unique.txt` | `reth-network` at `d763342f`: 430 packages, `revm 43.0.3` |
| `reth-network-v2.10.1.unique.txt` | `reth-network` at `58ebce14`: 442 packages, `evm2 0.1.0` by git rev `592bf18c` (`reth v2.10.1 Cargo.toml:434-437`, with a `[patch]` at `:671-673`), requires Rust 1.96 |

`net-spike` top level at `v2.3.0`: `reth-ecies`, `reth-network-peers` (feature `secp256k1`), `alloy-primitives 1.7.3`,
`alloy-rlp 0.3.17`, `secp256k1 0.30`, `snap 1.1.2`, `tokio`, `futures`. `reth-ecies` brings `secp256k1` (libsecp256k1, C code)
through its Cargo.toml (`crates/net/ecies/Cargo.toml`: `secp256k1 = { features = ["global-context", "std", "recovery", "rand"] }`).
The rest of xOne uses `k256` (RustCrypto) for ecrecover; the transport would be the only libsecp256k1 user.

Which tier to choose for `xone-net` when the toolchain moves: pin the lower crates (`ecies`, `peers`) of the newest tag
that the toolchain accepts. Taking `reth-eth-wire` or `reth-network` adds 100 to 300 packages and, from `v2.9.0` on, `evm2`
by git revision, and gives nothing here because of the blockers above.

## Build times (clean `target`, Linux, 20 cores, load average 38 to 76 during the runs, so wall times are upper bounds)

| Build | Wall | User CPU |
| --- | --- | --- |
| `cargo build --locked --tests -p net-spike` (reth `v2.3.0` ecies and peers, 135 packages) | 36 s | 116 s |
| `cargo build --locked --tests -p wide-balance-spike` (revm 43.0.3, for comparison) | 27 s | 101 s |
| `reth-network` at `v2.8.1` alone (`+nightly`, 430 packages, no node) | 51 s | 327 s |
| `go build ./cmd/geth` at the pin (cold build cache, warm module cache; second figure includes module download) | 3 min 05 s | 357 s |
| | (1 min 59 s) | (347 s) |
| `cargo clippy --locked --all-targets -- -D warnings`, whole workspace | 48 s | 118 s |

## Not covered

* Discovery v4 and v5, ENR fork-id filtering, the peer manager, peer scoring: not exercised. `reth-discv4` and `reth-discv5`
  were not built. geth-XDC accepts ENR-less peers on XDC chains (`eth/protocols/eth/discovery.go:66-80`), so ENR is not required for dialing.
* snap/1 client and server, `GetNodeData` fast sync (message `0x0d`/`0x0e`, `protocol.go:112-113`): not exercised.
* NewBlock, NewBlockHashes, transaction gossip, bodies and receipts: not exercised. eth/101 receipts use the eth/70 request
  id shape and eth/100 receipts are the bare list (`eth/protocols/eth/handler.go:234-312`); only headers were tested.
* A real legacy XDPoSChain peer, a public network, a fleet host: none used by design.
* `Timeout` and `SyncInfo` on the wire; a validator exchange (needs a validator, out of scope).
* A capability literally named `xdpos2` (see above).
* eth/63: advertised by geth for cap matching only; its handler is not run on XDC (`protocol.go:63-68`).
* The workspace pin is `v2.3.0` for toolchain reasons. A run of `local_geth.rs` at the `v2.8.1` pin was not made; the
  `ecies` and `peers` sources differ by a backpressure setting and a few `peers` lines, and the blocker proof was run at `v2.8.1`.
