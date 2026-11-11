# Discovery v4 with the XDC ping (packet type 5)

## Rule

1. A discv4 datagram is `hash(32) ++ signature(65) ++ type(1) ++ rlp(body)`. The hash is `keccak256(signature ++ type ++ body)`, the signature is over `keccak256(type ++ body)`, and the sender is recovered from it. At most 1,280 bytes. A bad hash, a bad signature or a wrong size drops the packet.
2. The ping is sent as packet type 5, not 1. On input, 5 and 1 both decode as a ping. Type 5 is never an ENR request. ENR is not required: type 6 (ENR response) is not answered or sent; an unknown type is dropped. Without type 5 a node finds 0 peers on XDC mainnet.
3. Ping `[4, from, to, expiration]` (`ENRSeq` is not sent), Pong `[to, pingHash, expiration]`, Findnode `[target(64), expiration]`, Neighbors `[[ip, udp, tcp, id]..], expiration]`; an endpoint is `[ip, udp, tcp]` with a 4- or 16-byte `ip`. Fields after the documented ones are ignored on input.
4. A packet whose expiration is in the past is dropped. We send expiration = now + 20 s. A pong must carry the hash of a ping we sent to that node and come from its address within 500 ms, or it is unsolicited.
5. A node enters the table only after a valid pong. A ping from an unknown node is answered with a pong, and then we ping back to bond.
6. Findnode is answered only for a node in the table at the same IP (no endpoint proof, no answer: this stops spoofed amplification). The reply is the 16 closest nodes in packets of at most 12. A Neighbors packet is accepted only after our Findnode to that node.
7. Nodes from Neighbors are pinged, not trusted. They are dropped if the UDP port is at most 1024, if the id is ours, or if the address is unspecified, multicast, or loopback or private when the relaying node is not on such a network.
8. The table has 17 buckets of 16 by log distance of `keccak256(id)` (distances up to 240 share the first bucket), at most 272 nodes. A full bucket keeps its nodes. At most 256 pings and 256 findnodes wait for an answer; more are not sent.

## Source

At the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `p2p/discover/v4wire/v4wire.go:38-53` types, `UseXDCPing`; `:55-111` bodies; `:113-114` `MaxNeighbors`; `:179-191` ping kind 5; `:229-271` decode (type 5 decoded as Ping); `:273-291` encode.
- `p2p/discover/v4_udp.go:52-64` response timeout, expiration, bond expiration, 1,280-byte packets; `:596-630` bond and `nodeFromRPC` (port above 1024, relay check); `:672-701` ping and the pong; `:703-713` ping back; `:722-757` pong and findnode verification; `:759-784` neighbors in chunks of 12; `:788-798` neighbors verification.
- `p2p/discover/table.go:45-56` bucket size 16, 17 buckets, `bucketMinDistance`; `p2p/netutil/net.go:197-222` relay rules.

## Networks

All. Public bootnodes are not probed in this item.

## Rust

`crates/xone-net/src/discv4.rs` (`encode`, `decode`, `Discv4`, `Table`; sans-IO, driven over a fake network in the tests `ping_is_type_5_and_bonding_fills_both_tables`, `findnode_needs_a_bond_and_returns_neighbors_that_get_pinged`, `bad_packets_are_dropped_without_a_reply`, `enr_types_ignored_and_standard_ping_accepted`, `table_is_bounded_and_ping_queue_is_bounded`). Datagrams equal the oracle's, signature included (`crates/xone-net/tests/wire.rs` `discv4_packets_equal_oracle_bytes`).

## Not covered

The UDP receive loop, periodic refresh and revalidation, ENR and discv5, and the public discovery probe. A local geth-XDC with `--nodiscover` does not answer discv4, so this page is checked against oracle bytes and a fake network only.
