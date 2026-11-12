# eth/100 and eth/101: versions, framing and message set

## Rule

1. XDC peers speak the capability named `eth`, at versions 100 (`XDPOS2`) and 101 (`XDPOS2RID`). There is no capability named `xdpos2`; the BFT messages are codes `0xe0` to `0xe2` of the same code space. A peer that offers only these is an `eth` peer whose version stock reth cannot parse.
2. We offer 101 first, then 100. Negotiation takes the highest version both sides offer. The version is fixed per session, and the framing follows it.
3. Version 100 (eth/63 shape, bare): `GetBlockHeaders` is `[origin, amount, skip, reverse]`; `BlockHeaders` is the bare list of headers; `GetBlockBodies` and `GetReceipts` are the bare list of hashes; `BlockBodies` and `Receipts` are bare lists. No request id.
4. Version 101: `GetBlockHeaders` is `[id, [origin, amount, skip, reverse]]`; `BlockHeaders` `[id, [headers]]`; `GetBlockBodies` `[id, [hashes]]`; `BlockBodies` `[id, [bodies]]`; `GetReceipts` `[id, firstBlockReceiptIndex, [hashes]]` and `Receipts` `[id, lastBlockIncomplete, [lists]]` (the eth/70 shape). Status, `NewBlock`, `NewBlockHashes`, `Transactions` and the BFT messages are the same in both versions.
5. Origin is a 32-byte string (hash) or an integer of at most 8 bytes (number). Any other length is an error.
6. Codes: `0x00` Status, `0x01` NewBlockHashes (`[[hash, number]..]`), `0x02` Transactions, `0x03` GetBlockHeaders, `0x04` BlockHeaders, `0x05` GetBlockBodies, `0x06` BlockBodies, `0x07` NewBlock (`[block, td]`), `0x08` and `0x09` (legacy DEX and lending transactions: drained undecoded), `0x0a` PooledTransactions, `0x0d` GetNodeData, `0x0e` NodeData, `0x0f` GetReceipts, `0x10` Receipts, `0x11` BlockRangeUpdate, `0xe0` Vote, `0xe1` Timeout, `0xe2` SyncInfo. The space has 229 codes; any other code is a protocol error and ends the session.
7. On the RLPx stream the eth codes start at offset 16. Every message is snappy-compressed from p2p version 5. The decompressed size is read from the snappy header and checked against the cap of the code before any allocation: Vote and Timeout 1 KiB, SyncInfo 32 KiB, Status 4 KiB, every other message 10 MiB (geth `maxMessageSize`). A message over its cap is an `Oversized` offence (100 points, immediate ban for an ordinary peer).
8. Transactions and receipts are carried as canonical raw RLP items; their content is checked by the stage that uses them (XB-14 and later). Strict RLP applies to everything: non-canonical integers or lengths, trailing bytes, nesting deeper than 32 are errors.
9. A message that does not decode ends the session (as in geth), whoever the peer is. BFT messages are the exception: geth drops a malformed one silently; we drop and score it (Malformed, 10 points) and the session ends at the threshold.
10. Serving bodies and receipts (`GetBlockBodies`, `GetReceipts`). The answer holds the blocks in the order asked, at most 1024, and stops once it holds 2 MiB (the block that crosses the limit is included), so one answer stays under the 10 MiB message cap of rule 7 (blocks of 20,000 transactions are 2 MiB or more each). It stops at the first hash the node does not hold: a node that retains none of the range (an anchored or minimal node below its retention floor, or an unknown hash) answers an empty list at once. It never answers a typed refusal and never stays silent, and it never skips a missing block, which would shift every later body against its hash. A client reads an empty or short answer as "not here, or not all" and asks again, or asks another peer. The read of an old block uses one MDBX read transaction and the segment files and takes neither the import lock nor the store lock, on a thread of its own.

## Source

At the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `eth/protocols/eth/protocol.go:34-46` constants; `:48-80` name `eth`, offer order `{101, 100, 70, 69, 63}`, why 100 stays; `:89` 229 codes; `:92` `maxMessageSize`; `:97-128` codes.
- `eth/protocols/eth/protocol.go:204-218` NewBlockHashes, NewBlock; `:225-273` header query and origin encoding; `:294-362` bodies, receipts, packets.
- `eth/protocols/eth/handler.go:231-310` handler maps of 100 and 101 (receipts of 101 use the eth/70 shape, `:297-303`); `:312-343` NewBlockHashes and NewBlock handlers; `:380-427` size check, version dispatch, unknown code error.
- `eth/protocols/eth/handlers.go:260-277` `ServiceGetBlockBodiesQuery` (stop at the first body it does not hold, `softResponseLimit` and `maxBodiesServe` checked before each lookup), `:350-373` the same for receipts of 100; `eth/protocols/eth/handler.go:37,50,56` the limits (2 MiB, 1024, 1024); `core/blockchain_reader.go:130-146` `GetBodyRLP` takes no chain lock. (The issue names `eth/handler.go`; the empty answer is in these two files.)
- `eth/protocols/eth/handlers.go:37-63` headers: bare for 100 only; `:238-256` bodies; `:291-298` receipts 101; `:323-344` receipts 100; `:300-321` codes `0x08` and `0x09` drained.
- `eth/protocols/eth/peer.go:447-458`, `:544-553` bare replies; `:673-690` bare request for 100 only.
- `p2p/peer.go:41-57` offset and base codes, `:438-459` `matchProtocols`; `p2p/transport.go:149` snappy from version 5; `p2p/rlpx/rlpx.go:146-157` size check before decompression.
- `eth/handler_eth.go:194-225` NewBlockHashes is announcement only; NewBlock is inserted when its parent is the head.

## Networks

All.

## Rust

`crates/xone-net/src/eth/{codes,mod,v100,v101}.rs`, `src/rlpx.rs`. Rule 10: `crates/xone-node/src/services/chain.rs` (`bodies_answer`, `receipts_answer`, `serve_blocking`), `Core::body`/`Core::receipts` and `xone_sync::mdbx::StoreReader::{body,receipts}`; tests `a_peer_that_does_not_hold_the_range_answers_an_empty_list` and `crates/xone-node/tests/follow.rs` `old_bodies_are_served_within_the_deadline_without_the_import_lock`. Byte-exact against 28 eth cases in `crates/xone-net/tests/wire.rs` (`every_message_round_trips_byte_exact`, `typed_values_equal_oracle_bytes`, `versions_use_their_framing`, `non_canonical_rlp_is_rejected`). Live: `crates/xone-net/tests/local_geth.rs`.
