# Penalty vectors (XB-10)

| file | what |
| --- | --- |
| `apothem-v2.json` | 30 Apothem V2 epoch switches: the last 10 before `TIPUpgradePenalty` (83,600,000), the first 10 from it, 10 around the first post-upgrade release (83,741,978) |
| `net2040-v2.json` | 7 net2040 epoch switches around its own `tipUpgradePenaltyBlock` 16,200 (no node is penalised there) |
| `apothem-v1.json` | 10 Apothem V1 checkpoints with a penalty, 3,631,500 to 50,609,700 |

V2 epoch switches of the Apothem archive (`xdc02:21105`, read-only, export date 2026-11-11, geth-XDC at `conformance/PIN`). Read by `crates/xone-consensus/tests/penalty_vectors.rs`. Never edit by hand: `build/`.

Record: `number` of the epoch-switch block, `prev` the previous epoch-switch block (the epoch that ends starts there), `round` of the parent header, `addrs` the address table the other fields index into, `parent_masternodes` (the `validators` of `prev`), `candidates` (the 0x88 candidate array at the gap block of the epoch, `eth_getStorageAt`), `history` (the `Penalties` of the epoch-switch headers `prev` and before, newest first, up to five), `miners` (the coinbase of each block `prev + 1 ..= number - 1`), `hash15` (the hash of every block of `prev ..= number - 1` that is a multiple of 15), `signs` (block, signer, signed block) of the sign transactions of the nodes listed in `history`, `header_penalties` (the `Penalties` field of the epoch-switch header, the expected value).

The other block hashes are not read by the scan, so the test fills them in. The test requires the derived list to equal `header_penalties` as a set (`CompareSignersLists`), for every vector, and checks that the released nodes are released by their sign transactions only.

## V1 record (`apothem-v1.json`)

`number` of the checkpoint, `signers` (`XDPoS_getSnapshot(number - 1).signers`, the candidates of the hook), `prev_masternodes` (the `extraData` list of the checkpoint at `number - 900`), `comeback_penalties` (the `Penalties` of the header at `number - 4500`, null below), `prior_penalties` (those of the headers at `number - 900 ..= number - 3600`, which `removePenaltiesFromBlock` takes out), `miners` (the sealer of each block `number - 899 ..= number - 1`, recovered from the seal with `ecrecover` over the seal hash and checked against the snapshot's `recents`), `hash15`, `signs`, `next` (the `extraData` list of the checkpoint, the expected value), `header_penalties` (the `Penalties` of the checkpoint header, empty on most Apothem V1 checkpoints). The test derives the penalties and requires `signers - penalties - prior == next` as a set, and the penalties to equal `header_penalties` where the header has any. The archive does not store the V1 penalty list itself, so this checks the penalties through the list they shorten.
