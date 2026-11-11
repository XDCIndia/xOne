# Header vectors

Real block headers from two geth-XDC archive nodes, one JSON object per line.
Written by `conformance/exporter headers`; never edit a vector by hand, run the
exporter again. Read by `crates/xone-primitives/tests/vectors.rs`.

| file | network | headers | size |
| --- | --- | --- | --- |
| `apothem.jsonl` | Apothem, chain 51 | 2000 | 8.2 MB |
| `net2040.jsonl` | net2040, chain 2040 | 500 | 1.6 MB |

## Line format

Every key of the `eth_getBlockByNumber(n, false)` answer, with its value as
returned (`hash` is the canonical block hash), except the body and derived keys
`transactions`, `uncles`, `withdrawals`, `size`, `totalDifficulty`. One key is
added, `xone`:

- `class`: why the block is in the sample (below).
- `consensus`: `v2` if the number is above the V2 switch block, else `v1`
  (geth `XDPoS.IsV2Block`).
- `sealHash`, `v2SigHash`, `hashNoValidator`: what the pinned geth-XDC returns
  for `SealHash`, `engine_v2.SignHash`, `Header.HashNoValidator`.
- `signer`: the address geth recovers (V1: `XDPoS.Author`, signature in the
  last 65 bytes of `extraData`; V2: `ecrecover` of `v2SigHash` and `validator`),
  `null` when the header has no signature (block 0).

The exporter stops if geth's own `Header.Hash()` differs from the archive hash.
Oracle: geth-XDC at `conformance/PIN` (`6fc8e8c2088b7f18264f5663036669d04fa7becf`).

## Archive and export

| | Apothem | net2040 |
| --- | --- | --- |
| `web3_clientVersion` | `XDC/v1.17.10-xdc.20-08d25caf-20261028/linux-amd64/go1.25.0` | not served; `admin_nodeInfo` name: `XDC/net2040-geth-archive-de6c/v1.17.10-xdc.21/linux-amd64/go1.25.0` |
| `eth_chainId` | `0x33` | `0x7f8` |
| head at export | 85,228,022 | 730,472 |
| export date | 2026-11-11 | 2026-11-11 |
| seed | 1 | 1 |

Requests are batches of 5 blocks, one batch per second.

```sh
# host xdc02; ports are the archives' local JSON-RPC ports
ssh -N -L 127.0.0.1:17105:127.0.0.1:21105 -L 127.0.0.1:17071:127.0.0.1:10071 xdc02 &
cd conformance/exporter   # needs conformance/geth-src from conformance/fetch-geth.sh
go run . headers -network apothem -rpc http://127.0.0.1:17105 -head 85228022 -total 2000 -out ../vectors/headers/apothem.jsonl
go run . headers -network net2040 -rpc http://127.0.0.1:17071 -head 730472  -total 500  -out ../vectors/headers/net2040.jsonl
```

## Block list rule

The list is a set of block numbers, sorted. The first rule that names a block
gives its `class`. Random draws use Go `math/rand` with `-seed` and skip blocks
already chosen. Fork rows, the V2 switch and the V2 epoch switches come from the
pinned geth config (Apothem: the built-in config, the same one that generates
`crates/xone-chainspec/generated/forks.json`; net2040: the config the node
reports in `admin_nodeInfo`).

1. `genesis`: block 0.
2. `early`: blocks 1 to 10.
3. `fork`: for every fork row with a block number (the `*Block` rows, the
   effective switches for Apothem, and the V2 switch) that is not the
   unscheduled sentinel 99999999999 and not above the head: row-1, row, row+1.
4. `epoch`: the V1 checkpoints `switch - k * 900` for k = 0 to N, and the first N
   V2 epoch switch blocks the node lists with `XDPoS_getEpochNumbersBetween`
   after the switch block. N is 20 for Apothem (switch 56,828,700) and 10 for
   net2040 (switch 10,800).
5. `latest`: head-49 to head.
6. Random draws:
   - Apothem: `v1` 200 in [1, switch]; `v2` 200 in [switch+1, 71,549,999] (before
     EIP-1559); `cancun` 200 in [71,551,800, 84,332,999]; `prague` 200 in
     [84,333,000, head].
   - net2040: `v1` 40 in [1, 10,800]; `v2` 60 in [10,801, 21,599]; `amsterdam`
     100 in [21,600, head].
7. `filler`: uniform draws in [1, head] until the list has `-total` blocks.

Vectors per class, as written:

| class | Apothem | net2040 |
| --- | --- | --- |
| genesis | 1 | 1 |
| early | 10 | 10 |
| fork | 30 | 39 |
| epoch | 39 | 9 |
| latest | 50 | 50 |
| v1 | 200 | 40 |
| v2 | 200 | 60 |
| cancun | 200 | |
| prague | 200 | |
| amsterdam | | 100 |
| filler | 1070 | 191 |
| total | 2000 | 500 |

A block that is also a fork row or epoch switch is counted once, under the
earlier rule. The Amsterdam rows of net2040 (the 7 optional header fields
present, `blockAccessListHash` set) are every block from 21,600.
