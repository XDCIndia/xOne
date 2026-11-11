# SkyNet heartbeat

Rust: `xone_node::observe::heartbeat` (`payload`, `validate`, `post`, `Heartbeat` service). Fixture: `crates/xone-node/tests/fixtures/heartbeat/ok.json`.

**Payload.** One JSON object POSTed to the configured URL every `interval` with `Content-Type: application/json`:

| key | type | source |
|---|---|---|
| `blockHeight` | number, or `null` before the node has a head | head number |
| `peerCount` | number | `PeerStats::peer_count` |
| `isSyncing` | bool | `ChainStats::syncing` |

The keys are camelCase because SkyNet drops other spellings without an error (issue #41, operator record). `validate` is the schema test: exactly these three keys, these types, nothing else; `block_height` and friends fail it.

**Unverified.** The SkyNet ingestion code is not in this repository, and no other file in `xOneGo/pkg/node` mentions SkyNet. The issue names only the three keys, so only those are sent. The types (numbers, not strings) and any further required key (a node name or client version would be a guess) are **not verified**; confirm against the ingestion code and extend `payload` and `validate` together. Authentication is likewise unknown: `authorization` is an optional header value from the env file, never printed.

**Bounds.** TLS by default (`https://`); `http://` needs `allow_plaintext`. One request per interval through a `TokenBucket` (burst 3, 1 token per second, cost = interval seconds), no early retry after a failure, 3 s connect and 10 s socket timeouts, own thread. Success is any 2xx status line.
