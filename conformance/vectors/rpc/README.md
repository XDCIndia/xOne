# RPC vectors (XB-22)

The answers of the geth-XDC archives (Apothem `xdc02:21105`, net2040 `xdc02:10071`, read-only, 4 HTTP requests per second, at most 5 calls per request, geth-XDC at `conformance/PIN`) to every in-scope method, at 50 blocks per network spread over the fork rows, the V1 to V2 switch and the epoch switches, plus the "live" calls (head, node, peers, probes) once. One file per method, `<network>/<method>.jsonl`, one line per call: `{network, block, class, request, response}`. The `response` is the archive's JSON verbatim, errors included; a response over 1 MiB is not stored (`omitted`). Read by `conformance/rpc-parity`. Never edit a vector by hand: run `build/build.py` again.

Apothem also holds eight blocks of the replayed window that carry transactions (`build.py apothem 16,17,...`), because the sweep can only compare receipts, transactions and traces on blocks the harness has executed.

Replay: `cargo run -p xone-rpc-parity -- --network apothem|net2040|all`; the machine-readable report is written to `conformance/rpc-parity/report/<network>.json`.
