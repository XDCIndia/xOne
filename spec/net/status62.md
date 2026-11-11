# Status of eth/100 and eth/101: five fields, no ForkID

## Rule

1. The status message (code `0x00`) of versions 100 and 101 is the list `[version, networkId, td, head, genesis]`. There is no ForkID and nothing after `genesis`. We send exactly these five fields: a legacy peer with a strict five-field decoder drops a longer list.
2. A received status is checked in this order: network id equal to ours; protocol version equal to the negotiated version; genesis hash equal to ours. The first failure ends the handshake (Disconnect, reason `0x10`).
3. `td` is a ranking value only. It is the peer's total difficulty (the geth comment calls it a block number; on the fleet it is the real TD). We saturate it to `u64` and use it to prefer one peer over another. It is never compared with a block number and never rejects a peer. A zero `head` is not an error.
4. A ForkID is never required and never rejected. A received status with one extra trailing element is accepted and the element is skipped unread; whatever it holds, the peer is not refused for it. Two or more extra elements are an error. (Regression for XDCIndia/xOneGo#684: a fresh modern client had 0 peers on the same bootnodes because of a fork id mismatch.)
5. Integers are canonical. A leading zero byte, a one-byte string below `0x80` and a long-form length below 56 are errors.
6. The first message after Hello must be the status, at most 4 KiB (ADR-007 section 5; geth: 10 MiB). Both sides send at once, so the handshake as a whole is bounded by 5 seconds.

## Source

At the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `eth/protocols/eth/protocol.go:196-202` `StatusPacket62` (five fields); `:186-195` the 5-field rule and why no caps trailer is sent.
- `eth/protocols/eth/handshake.go:110-116` versions 100 and 101 take `handshake62`; `:149-176` send and read; `:178-190` the three checks (`errNetworkIDMismatch`, `errProtocolVersionMismatch`, `errGenesisMismatch`); `:191-214` TD stored as a rank only; `:215-235` no `Validate`, no ForkID check.
- `eth/protocols/eth/handshake.go:77` handshake timeout 5 s; `:267-283` first message must be `StatusMsg`, size at most `maxMessageSize`.
- `eth/protocols/eth/handshake.go:118-147`, `:238-265` the modern status with ForkID, used only by eth/69 and 70, never by XDC peers.

## Networks

All (50, 51, 5551, 551). The network id and genesis are the chain's.

## Rust

`crates/xone-net/src/eth/status62.rs` (`Status62`, `Local`, `validate`, tests `fork_id_is_never_required_nor_rejected`, `validation_order_and_rank`, `non_canonical_and_short_status_rejected`). Over the wire: `crates/xone-net/tests/peers.rs` `handshake_is_bounded_and_status_is_validated`. Oracle vectors: `status62`, `status62_td_wide` in `conformance/vectors/net/vectors.json`.

## Legacy

Our decoder is more tolerant than geth's on one point (one trailing element). That is deliberate: it can only make us accept a peer, never accept a different chain.
