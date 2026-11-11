# Gossip of Vote, Timeout and SyncInfo

## Rule

1. A Vote (`0xe0`), Timeout (`0xe1`) and SyncInfo (`0xe2`) is the bare RLP of its type, with no wrapper and no request id, in both versions.
2. A message is identified by `keccak256` of its RLP (geth `Hash()` is `rlpHash`), so two messages that differ in any field, the signature included, are two messages. We accept only canonical bytes, so the hash of the bytes is the hash of the message.
3. Order of checks on receipt: size cap (before decoding), strict decode, then dedup. Dedup is by hash against a bounded global set (32,768 hashes) and marks the sender as knowing it in a bounded per-peer set (1,024). A duplicate is dropped without penalty.
4. A new message goes to the consensus driver once. After the driver has verified it, the relay sends it to every connected peer that has not seen it, except the sender; each peer is marked when sent to, so a peer receives a message at most once. A message we create goes to every peer once and is remembered globally so that its echo is a duplicate.
5. Malformed (does not decode, wrong code, trailing byte, not canonical) scores 10; over the cap scores 100 (Vote and Timeout 1 KiB, SyncInfo 32 KiB). Past 100 the peer is disconnected. geth drops a malformed message without penalty; the score is ADR-007 section 5.
6. Memory is bounded: when a set is full the oldest hash is forgotten. A message older than the set can then be delivered again; consensus discards stale rounds anyway.
7. The per-peer inbound queue for BFT messages is 256 messages and drops the oldest when full (ADR-007 Q1). A vote for an old round is worth less than the newest.

## Source

At the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `eth/protocols/eth/protocol.go:125-127` codes; `:528-551` bodies are raw RLP.
- `eth/protocols/eth/handlers.go:1157-1185` raw decode and hand-off.
- `eth/handler_eth.go:137-185` decode, malformed is ignored (`:148-149`), hand to the BFT dispatcher.
- `eth/bft/handler.go:32-38` known caches (131,072 each), `:46-56` forward queue of 1,024, `:118-125` caches keyed by message hash, `:185-229` Vote: distance guard, dedup before verify, forward after verify; `:233-261` Timeout; `core/types/consensus_v2.go:71-73,112-114,149-151` `Hash()` is `rlpHash`.
- `eth/handler_xdc.go:23-100` broadcast to all peers: Vote, Timeout and SyncInfo are sent as the struct.

## Networks

Devnet, Apothem and mainnet after the V2 switch.

## Rust

`crates/xone-net/src/bft_gossip.rs` (`Gossip`, `Known`; tests `each_message_reaches_each_peer_once`, `own_message_is_sent_once_and_its_echo_is_a_duplicate`, `size_caps_malformed_and_non_canonical_are_scored`, `memory_is_bounded`), `crates/xone-net/src/peers/mod.rs` (`broadcast_bft`, `relay_bft`, queue Q1), `crates/xone-net/tests/peers.rs` `bft_messages_are_delivered_once_to_the_app_and_once_to_each_peer`. Vote semantics (round and distance guards, signatures) are XB-15 and are not here.

## Not covered

The distance guards (`maxBlockDist` 7, `3 * epoch`) belong to the consensus driver. The `NewBlock` and `NewBlockHashes` broadcast is not implemented; they are received, decoded and queued (Q3, 16 per peer).
