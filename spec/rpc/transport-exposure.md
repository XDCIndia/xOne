# RPC transport exposure (issue #60)

One contract for HTTP, WebSocket and IPC. Source: `crates/xone-rpc/src/{policy,transport}.rs`,
`crates/xone-node/src/services/rpc.rs`. geth references at the oracle pin `6fc8e8c2`:
`node/rpcstack.go` (`virtualHostHandler`, `enableRPC`, `enableWS`), `rpc/websocket.go`
(`wsHandshakeValidator`), `node/jwt_handler.go`, `rpc/ipc_unix.go`.

## Matrix

| | HTTP | WebSocket | IPC |
| --- | --- | --- | --- |
| enabled by default | no (`--http`) | no (`--ws`) | yes (`--ipcdisable` turns it off) |
| bind default | `localhost:8545` | `localhost:8546` | `<datadir>/xone.ipc`, mode `0600` |
| namespaces | `--http.api`, default `eth,net,web3` | `--ws.api`, default `eth,net,web3` | all served namespaces (local, owner only) |
| `admin`, `debug` | only when listed | only when listed | yes |
| unknown namespace or `*` | start refused | start refused | n/a |
| Host check | `--http.vhosts`, default `localhost`; an IP is accepted | none (geth) | n/a |
| Origin check | none (CORS is not authentication) | `--ws.origins`, default `http://localhost`; no `Origin` passes | n/a |
| authentication | `--rpc.jwtsecret`: `Authorization: Bearer <HS256 JWT>`, `iat` within 60 s; checked before the method or body is read | the same token on the upgrade request; a refused upgrade gets no socket | file mode `0600` |
| forwarding headers | `X-Forwarded-*`, `Forwarded`, `X-Real-IP` are ignored: they grant nothing | ignored | n/a |
| body / message | `limits.rpc_body_bytes` (5 MiB) | the same bound on a message and a frame | the same bound on the read buffer |
| batch, classes, deadlines, buckets | `Rpc::serve` | `Rpc::serve` | `Rpc::serve` |
| connections | 256, 64 in flight, 128 waiting | 128, 8 in flight each, out buffer 1,024 | 16, 8 in flight each |

Refusals: wrong `Host` or `Origin` is `403`, missing or invalid credentials `401` (the reason is not
told), a method outside the transport's list `-32601`. None of them reaches a handler.

## Operator notes

* `--rpc.jwtsecret <file>`: 32 bytes as hex, `0x` optional. A missing or malformed file stops the
  start; nothing is generated. It is separate from `--authrpc.jwtsecret`, which `ops/run-xone.sh`
  passes for the (unserved) engine port and which does not change HTTP or WebSocket.
* Remote privileged access needs three explicit choices: a non-local `--http.addr`/`--ws.addr`,
  `admin` or `debug` in the API list and, for any exposure, `--rpc.jwtsecret`. No default opens it.
* `dumpconfig` prints the ethstats secret and URL credentials as `<redacted>`
  (`Config::redacted`). Secret file paths are printed: they are not secrets. The IPC socket is
  chmod-ed right after bind, as geth does; the short window before it is the documented limit.

## Tests

`crates/xone-rpc/tests/rpc.rs`: `http_with_a_jwt_secret_refuses_every_bad_credential_before_dispatch`,
`websocket_with_a_jwt_secret_needs_the_token_on_the_upgrade`,
`websocket_shares_the_http_body_and_batch_limits`,
`a_plain_http_request_to_the_websocket_port_is_not_dispatched`, `ipc_serves_a_json_stream` (mode),
`http_refuses_methods_outside_its_list_and_a_wrong_host`,
`websocket_refuses_methods_outside_its_list_and_a_wrong_origin`; `policy::tests::jwt_rules`.
`crates/xone-node/tests/rpc_security.rs` runs the packaged entry point (`services::build`).
`crates/xone-node/src/config.rs` `redacted_hides_the_ethstats_secret_and_url_credentials`.
