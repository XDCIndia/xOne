# xone flags

Generated from `bin/xone/src/flags.rs` by the `flags_doc_is_current` test; do not edit by hand (`UPDATE_DOCS=1 cargo test -p xone flags_doc` rewrites it).

Every flag takes `--name` or `-name` (geth style), `--name value` or `--name=value`. A boolean is `--name` or `--name=false`. A flag always wins over the `--config` file. An unknown flag exits 2.

Source column: file:line in geth-XDC (`XDCIndia/go-ethereum` at `6fc8e8c2`; `flags.go` is `cmd/utils/flags.go`), then the `join.sh` launch lines that pass the flag (ItWorksinMyLocal `24b915e`, geth line 570, xone line 682).

| Flag | Aliases | Default | Meaning | Source |
|---|---|---|---|---|
| `--datadir <path>` |  | xone-data | Data directory for the databases and the chain marker. | flags.go:93 ; geth,xone |
| `--networkid <n>` |  | chain id of the profile or genesis | Network id. Must equal the chain id of the genesis in use. | flags.go:141 ; geth,xone |
| `--syncmode snap|fast|full` |  | snap | Blockchain sync mode. | flags.go:299 ; geth,xone |
| `--gcmode full|archive` |  | full | State garbage collection mode. | flags.go:338 |
| `--syncfromblock <n>` |  | unset | First block to import; unset means from genesis. xone-defined. | not in geth-XDC at the pin; task list |
| `--cache <n>` |  | 4096 | Megabytes of memory for internal caches. | flags.go:556 |
| `--history.transactions <n>` | `-txlookuplimit` (deprecated) | 2350000 | Recent blocks whose transactions stay indexed; 0 is the whole chain. | flags.go:379 |
| `--port <n>` |  | 30303 | P2P listening port (TCP). | flags.go:969 ; geth,xone |
| `--discovery.port <n>` |  | same as port | P2P discovery port (UDP). | flags.go:1026 ; xone |
| `--maxpeers <n>` |  | 50 | Maximum peers; 0 disables networking. | flags.go:957 ; geth,xone |
| `--nodiscover` |  | false | Disable peer discovery. | flags.go:997 |
| `--nat <string>` |  | unset | NAT port mapping: any, none, upnp, pmp, extip:<IP>. | flags.go:991 ; geth,xone |
| `--bootnodes <a,b,..>` |  | built-in for the profile | Comma-separated enode URLs for discovery bootstrap. | flags.go:975 ; geth,xone |
| `--identity <string>` |  | unset | Node name shown to peers and ethstats. | flags.go:199 ; geth |
| `--http` |  | false | Enable the HTTP-RPC server. | flags.go:829 ; geth,xone |
| `--http.addr <string>` |  | localhost | HTTP-RPC listen address. | flags.go:834 ; geth,xone |
| `--http.port <n>` |  | 8545 | HTTP-RPC listen port. | flags.go:840 ; geth,xone |
| `--http.api <a,b,..>` |  | eth,net,web3 | APIs offered over HTTP-RPC. | flags.go:858 ; geth,xone |
| `--http.corsdomain <a,b,..>` |  | none | Allowed CORS origins for HTTP-RPC. | flags.go:846 ; geth |
| `--http.vhosts <a,b,..>` |  | localhost | Allowed virtual hosts for HTTP-RPC. | flags.go:852 ; geth,xone |
| `--ws` |  | false | Enable the WebSocket-RPC server. | flags.go:887 ; geth |
| `--ws.addr <string>` |  | localhost | WebSocket-RPC listen address. | flags.go:892 ; geth |
| `--ws.port <n>` |  | 8546 | WebSocket-RPC listen port. | flags.go:898 ; geth |
| `--ws.api <a,b,..>` |  | eth,net,web3 | APIs offered over WebSocket-RPC. | flags.go:904 ; geth |
| `--ws.origins <a,b,..>` |  | none | Allowed origins for WebSocket-RPC. | flags.go:910 ; geth |
| `--authrpc.addr <string>` |  | localhost | Authenticated RPC listen address. | flags.go:761 |
| `--authrpc.port <n>` |  | 8551 | Authenticated RPC listen port. | flags.go:767 ; geth,xone |
| `--authrpc.vhosts <a,b,..>` |  | localhost | Allowed virtual hosts for authenticated RPC. | flags.go:773 |
| `--authrpc.jwtsecret <path>` |  | unset | Path of the JWT secret file for authenticated RPC. | flags.go:779 |
| `--ipcdisable` |  | false | Disable the IPC server. | flags.go:819 |
| `--ipcpath <path>` |  | <datadir>/xone.ipc | IPC socket path. | flags.go:824 |
| `--mine` |  | false | Enable block production (deprecated spelling kept for XDC tooling). | flags_legacy.go:125 |
| `--miner.etherbase <string>` |  | unset | Address that receives block rewards. | flags_legacy.go:119 |
| `--unlock <a,b,..>` |  | none | Accounts to unlock, comma-separated. | flags_legacy.go:144 |
| `--password <path>` |  | unset | File with the account password; the password is never a flag. | flags.go:672 |
| `--keystore <path>` |  | unset | Keystore file of the validator key. Block production (--mine) needs it with --password, and is refused on chain ids 50 and 51 in this build. | flags.go:124 ; xone |
| `--ethstats <string>` |  | unset | Stats server as name:secret@host:port. | flags.go:786 ; geth,xone |
| `--verbosity <n>` |  | 3 | Log level: 0 silent, 1 error, 2 warn, 3 info, 4 debug, 5 detail. | internal/debug/flags.go:42 |
| `--override.genesis <path>` |  | unset | Genesis file; the datadir is initialised from it on first start. | flags.go:294 ; xone |
| `--chainspec <path>` |  | unset | Fork table of a private chain (one network, the format of generated/forks.json); refused for chain ids 50 and 51. | not in geth-XDC; xone-defined |
| `--xdcmainnet` |  | false | XDC mainnet profile (chain 50). | flags.go:166 |
| `--apothem` |  | false | XDC Apothem profile (chain 51). | flags.go:171 |
| `--xdcdevnet` |  | false | XDC devnet profile (chain 5551). | flags.go:176 |
| `--config <path>` |  | unset | TOML config file; flags override it. Not a config key. | cmd/geth/config.go:65 |
| `--metrics` |  | false | Enable the metrics endpoint. | flags.go:1074 |
| `--metrics.addr <string>` |  | 127.0.0.1 | Metrics listen address. | flags.go:1083 |
| `--metrics.port <n>` |  | 6060 | Metrics listen port. | flags.go:1088 |
| `--pprof` |  | false | Enable the profiling endpoint. | internal/debug/flags.go:106 |
| `--pprof.addr <string>` |  | 127.0.0.1 | Profiling listen address. | internal/debug/flags.go:117 |
| `--pprof.port <n>` |  | 6060 | Profiling listen port. | internal/debug/flags.go:111 |
| `--version` | `-V` | false | Print the version stamp and exit; never opens the datadir. Short form -V. | join.sh:481 (xOneGo lesson) |

Subcommands: `init <genesis.json>`, `version`, `dumpconfig`, `genesis-hash --network <name>`, `import <file>`, `export <file> [from] [to] --input <blockfile>`.
