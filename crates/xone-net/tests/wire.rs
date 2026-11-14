#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Byte-exact check of every eth/100, eth/101, base p2p and discv4 message against
//! `conformance/vectors/net/vectors.json`, which `conformance/vectors/net/oracle/run.sh` prints
//! from the pinned geth-XDC types. Offline. Non-canonical RLP is rejected.

use alloy_primitives::{Bytes, B256, B512, U256};
use alloy_rlp::Decodable;
use serde_json::Value;
use xone_net::eth::{
    self, codes, v100, v101, BftMessage, BlockAnnounce, Framing, GetHeaders, Message, Origin, Raw,
    Status62,
};
use xone_net::rlpx::{Cap, DisconnectReason, Hello};
use xone_primitives::{BlockInfo, Header, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote};

fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../conformance/vectors/net/vectors.json"
    ))
    .unwrap()
}

fn vec_bytes(name: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(vectors()[name].as_str().unwrap()).unwrap()
}

/// `common.Hash{b, 0xaa, 0xbb}` in the oracle.
fn h(b: u8) -> B256 {
    let mut x = [0u8; 32];
    x[..3].copy_from_slice(&[b, 0xaa, 0xbb]);
    B256::from(x)
}

fn sig() -> Bytes {
    (0..65u8).collect::<Vec<_>>().into()
}

fn info() -> BlockInfo {
    BlockInfo {
        hash: h(1),
        round: 7,
        number: 1234,
    }
}

fn header() -> Header {
    Header {
        parent_hash: h(10),
        uncle_hash: alloy_primitives::b256!(
            "1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347"
        ),
        coinbase: alloy_primitives::Address::from_slice(&{
            let mut a = [0u8; 20];
            a[..2].copy_from_slice(&[0x11, 0x22]);
            a
        }),
        state_root: h(11),
        tx_root: h(12),
        receipt_root: h(13),
        bloom: {
            let mut b = [0u8; 256];
            b[..3].copy_from_slice(&[1, 2, 3]);
            b.into()
        },
        difficulty: U256::from(1),
        number: 1234,
        gas_limit: 420_000_000,
        gas_used: 21_000,
        timestamp: 1_700_000_000,
        extra_data: Bytes::from_static(&[2, 0xc0]),
        validators: Bytes::from_static(&[0xaa, 0xbb]),
        validator: sig(),
        ..Default::default()
    }
}

/// Every eth vector: name, framing, code. `decode` then `encode` must give the oracle's bytes.
const ETH: &[(&str, Framing, u64)] = &[
    ("status62", Framing::Bare, codes::STATUS),
    ("status62", Framing::RequestId, codes::STATUS),
    ("status62_td_wide", Framing::Bare, codes::STATUS),
    ("new_block_hashes", Framing::Bare, codes::NEW_BLOCK_HASHES),
    (
        "new_block_hashes",
        Framing::RequestId,
        codes::NEW_BLOCK_HASHES,
    ),
    ("transactions", Framing::Bare, codes::TRANSACTIONS),
    ("transactions", Framing::RequestId, codes::TRANSACTIONS),
    ("get_headers_bare", Framing::Bare, codes::GET_BLOCK_HEADERS),
    (
        "get_headers_rid",
        Framing::RequestId,
        codes::GET_BLOCK_HEADERS,
    ),
    (
        "get_headers_hash_rid",
        Framing::RequestId,
        codes::GET_BLOCK_HEADERS,
    ),
    ("block_headers_bare", Framing::Bare, codes::BLOCK_HEADERS),
    (
        "block_headers_rid",
        Framing::RequestId,
        codes::BLOCK_HEADERS,
    ),
    ("get_bodies_bare", Framing::Bare, codes::GET_BLOCK_BODIES),
    (
        "get_bodies_rid",
        Framing::RequestId,
        codes::GET_BLOCK_BODIES,
    ),
    ("block_bodies_bare", Framing::Bare, codes::BLOCK_BODIES),
    ("block_bodies_rid", Framing::RequestId, codes::BLOCK_BODIES),
    ("new_block", Framing::Bare, codes::NEW_BLOCK),
    ("new_block", Framing::RequestId, codes::NEW_BLOCK),
    ("get_receipts_bare", Framing::Bare, codes::GET_RECEIPTS),
    ("get_receipts_rid", Framing::RequestId, codes::GET_RECEIPTS),
    ("receipts_bare", Framing::Bare, codes::RECEIPTS),
    ("receipts_rid", Framing::RequestId, codes::RECEIPTS),
    ("vote", Framing::Bare, codes::VOTE),
    ("vote", Framing::RequestId, codes::VOTE),
    ("timeout", Framing::Bare, codes::TIMEOUT),
    ("timeout", Framing::RequestId, codes::TIMEOUT),
    ("sync_info", Framing::Bare, codes::SYNC_INFO),
    ("sync_info", Framing::RequestId, codes::SYNC_INFO),
];

#[test]
fn every_message_round_trips_byte_exact() {
    for &(name, f, code) in ETH {
        let want = vec_bytes(name);
        let msg = eth::decode(f, code, &want).unwrap_or_else(|e| panic!("{name} {f:?}: {e}"));
        assert_eq!(msg.code(), code, "{name}");
        let (c, got) = eth::encode(f, &msg).unwrap();
        assert_eq!(
            (c, got),
            (code, want),
            "{name} {f:?}: re-encode differs from geth"
        );
    }
}

#[test]
fn versions_use_their_framing() {
    // 100 reads and writes the bare shape, 101 the request-id shape: same module API.
    let bare = vec_bytes("get_headers_bare");
    let rid = vec_bytes("get_headers_rid");
    let q = GetHeaders {
        origin: Origin::Number(0),
        amount: 1,
        skip: 0,
        reverse: false,
    };
    assert_eq!(
        v100::decode(codes::GET_BLOCK_HEADERS, &bare).unwrap(),
        Message::GetBlockHeaders {
            id: None,
            query: q.clone()
        }
    );
    assert_eq!(
        v101::decode(codes::GET_BLOCK_HEADERS, &rid).unwrap(),
        Message::GetBlockHeaders {
            id: Some(0x1122334455),
            query: q.clone()
        }
    );
    assert_eq!(
        v101::encode(&v101::decode(codes::GET_BLOCK_HEADERS, &rid).unwrap())
            .unwrap()
            .1,
        rid
    );
    // wrong framing for the version fails instead of guessing
    assert!(v100::decode(codes::GET_BLOCK_HEADERS, &rid).is_err());
    assert!(v101::decode(codes::GET_BLOCK_HEADERS, &bare).is_err());
    let with_id = Message::GetBlockHeaders {
        id: Some(1),
        query: q.clone(),
    };
    let without = Message::GetBlockHeaders { id: None, query: q };
    assert!(v100::encode(&with_id).is_err() && v101::encode(&without).is_err());
    assert_eq!((v100::VERSION, v101::VERSION), (100, 101));
}

#[test]
fn typed_values_equal_oracle_bytes() {
    let want = |n: &str| vec_bytes(n);
    let s = Status62 {
        version: 101,
        network_id: 5551,
        td: U256::from(1),
        head: h(2),
        genesis: h(3),
    };
    assert_eq!(alloy_rlp::encode(&s), want("status62"));
    assert_eq!(
        alloy_rlp::encode(vec![
            BlockAnnounce {
                hash: h(1),
                number: 5
            },
            BlockAnnounce {
                hash: h(2),
                number: 6
            }
        ]),
        want("new_block_hashes")
    );
    let hdrs = vec![header(), header()];
    assert_eq!(alloy_rlp::encode(&hdrs), want("block_headers_bare"));
    assert_eq!(
        eth::decode(
            Framing::RequestId,
            codes::BLOCK_HEADERS,
            &want("block_headers_rid")
        )
        .unwrap(),
        Message::BlockHeaders {
            id: Some(7),
            headers: hdrs
        }
    );
    // hash origin, skip and reverse
    assert_eq!(
        eth::decode(
            Framing::RequestId,
            codes::GET_BLOCK_HEADERS,
            &want("get_headers_hash_rid")
        )
        .unwrap(),
        Message::GetBlockHeaders {
            id: Some(1),
            query: GetHeaders {
                origin: Origin::Hash(h(4)),
                amount: 192,
                skip: 3,
                reverse: true
            }
        }
    );
    assert_eq!(
        eth::decode(
            Framing::Bare,
            codes::GET_BLOCK_BODIES,
            &want("get_bodies_bare")
        )
        .unwrap(),
        Message::GetBlockBodies {
            id: None,
            hashes: vec![h(1), h(2)]
        }
    );
    assert_eq!(
        eth::decode(
            Framing::RequestId,
            codes::GET_RECEIPTS,
            &want("get_receipts_rid")
        )
        .unwrap(),
        Message::GetReceipts {
            id: Some(10),
            first_index: 2,
            hashes: vec![h(1), h(2)]
        }
    );
    match eth::decode(Framing::RequestId, codes::RECEIPTS, &want("receipts_rid")).unwrap() {
        Message::Receipts {
            id,
            last_incomplete,
            per_block,
        } => {
            assert_eq!((id, last_incomplete, per_block.len()), (Some(11), true, 1));
        }
        m => panic!("{m:?}"),
    }
    match eth::decode(Framing::Bare, codes::NEW_BLOCK, &want("new_block")).unwrap() {
        Message::NewBlock(b) => {
            assert_eq!(b.block.header, header());
            assert_eq!(
                (b.td, b.block.transactions.len(), b.block.uncles.len()),
                (U256::from(1235), 2, 0)
            );
        }
        m => panic!("{m:?}"),
    }
    match eth::decode(
        Framing::Bare,
        codes::BLOCK_BODIES,
        &want("block_bodies_bare"),
    )
    .unwrap()
    {
        Message::BlockBodies { bodies, .. } => {
            assert_eq!((bodies.len(), bodies[0].transactions.len()), (1, 2));
            assert_eq!(bodies[0].uncles, vec![header()]);
        }
        m => panic!("{m:?}"),
    }
    // the typed (EIP-2718) transaction is a string inside the list, the legacy one a list
    match eth::decode(Framing::Bare, codes::TRANSACTIONS, &want("transactions")).unwrap() {
        Message::Transactions(t) => assert!(t[0].0[0] >= 0xc0 && (0x80..0xc0).contains(&t[1].0[0])),
        m => panic!("{m:?}"),
    }
}

#[test]
fn bft_messages_equal_oracle_bytes() {
    let want = |n: &str| vec_bytes(n);
    let vote = Vote {
        proposed_block_info: info(),
        signature: sig(),
        gap_number: 450,
    };
    assert_eq!(alloy_rlp::encode(&vote), want("vote"));
    let timeout = Timeout {
        round: 9,
        signature: sig(),
        gap_number: 450,
    };
    assert_eq!(alloy_rlp::encode(&timeout), want("timeout"));
    let qc = QuorumCert {
        proposed_block_info: info(),
        signatures: vec![sig(), sig()[..64].to_vec().into()],
        gap_number: 450,
    };
    let tc = TimeoutCert {
        round: 9,
        signatures: vec![sig()],
        gap_number: 450,
    };
    let si = SyncInfo {
        highest_quorum_cert: qc,
        highest_timeout_cert: tc,
    };
    assert_eq!(alloy_rlp::encode(&si), want("sync_info"));
    for (v, c) in [
        (BftMessage::Vote(vote), codes::VOTE),
        (BftMessage::Timeout(timeout), codes::TIMEOUT),
        (BftMessage::SyncInfo(si), codes::SYNC_INFO),
    ] {
        let body = v.encode_body();
        assert_eq!(BftMessage::decode_body(c, &body).unwrap(), v);
        assert!(body.len() <= codes::max_size(c));
        // trailing byte and a truncated body are errors
        let mut longer = body.clone();
        longer.push(0);
        assert!(BftMessage::decode_body(c, &longer).is_err());
        assert!(BftMessage::decode_body(c, &body[..body.len() - 1]).is_err());
    }
}

#[test]
fn hello_and_disconnect_equal_oracle_bytes() {
    let hello = Hello::decode(&mut &vec_bytes("hello")[..]).unwrap();
    assert_eq!(hello.version, 5);
    assert_eq!(hello.name, "XDC/v1.17.10-xdc.21/linux-amd64/go1.25.2");
    assert_eq!(hello.caps, vec![Cap::eth(101), Cap::eth(100)]);
    assert_eq!(hello.id, B512::ZERO);
    assert_eq!(alloy_rlp::encode(&hello), vec_bytes("hello"));
    assert_eq!(
        DisconnectReason::Quitting.encode(),
        vec_bytes("disconnect_quit")
    );
    assert_eq!(
        DisconnectReason::Requested.encode(),
        vec_bytes("disconnect_requested")
    );
}

/// A canonical item is the only accepted shape of an integer, a string length and a list length.
#[test]
fn non_canonical_rlp_is_rejected() {
    let status = vec_bytes("status62");
    let ok = |b: &[u8]| eth::decode(Framing::Bare, codes::STATUS, b).is_ok();
    assert!(ok(&status));
    // network id 5551 (82 15 af) with a leading zero byte, and as a long-form integer
    let at = status
        .windows(3)
        .position(|w| w == [0x82, 0x15, 0xaf])
        .unwrap();
    let mut leading_zero = status.clone();
    leading_zero[at] = 0x83;
    leading_zero.insert(at + 1, 0);
    leading_zero[1] += 1;
    assert!(!ok(&leading_zero));
    // version 101 (0x65) written as a one-byte string (81 65): single bytes below 0x80 are the byte
    let mut one_byte = status.clone();
    one_byte.splice(2..3, [0x81, 0x65]);
    one_byte[1] += 1;
    assert!(!ok(&one_byte));
    // a list whose long-form length is below 56 (f8 05 ...)
    assert!(eth::decode(Framing::Bare, codes::GET_BLOCK_BODIES, &[0xf8, 0x01, 0x80]).is_err());
    // a string length with a leading zero byte
    assert!(eth::decode(
        Framing::Bare,
        codes::TRANSACTIONS,
        &[0xc3, 0xb9, 0x00, 0x01]
    )
    .is_err());
    // an opaque item is walked: a non-canonical element nested in a transaction is rejected
    assert!(Raw::decode(&mut &[0xc2, 0x81, 0x05][..]).is_err());
    assert!(Raw::decode(&mut &[0xc2, 0x05, 0x80][..]).is_ok());
    // origin must be 32 bytes or at most 8
    let nine = [&[0xc9 + 8][..], &[0x89], &[1u8; 9], &[1, 0, 0]].concat();
    assert!(eth::decode(Framing::Bare, codes::GET_BLOCK_HEADERS, &nine).is_err());
    // trailing byte after a message
    let mut t = status.clone();
    t.push(0);
    assert!(!ok(&t));
    // unknown codes: reserved space and past the 229 messages
    assert!(matches!(
        eth::decode(Framing::Bare, 0x0b, &[0xc0]),
        Err(eth::WireError::UnknownCode(0x0b))
    ));
    assert!(eth::decode(Framing::Bare, codes::MESSAGE_COUNT, &[0xc0]).is_err());
    // nesting deeper than 32 is refused without recursion overflow; 20 levels are fine
    let nested = |levels: usize| {
        let mut item = vec![0xc0];
        for _ in 0..levels {
            let mut next = Vec::new();
            alloy_rlp::Header {
                list: true,
                payload_length: item.len(),
            }
            .encode(&mut next);
            next.extend(item);
            item = next;
        }
        item
    };
    assert!(Raw::decode(&mut &nested(20)[..]).is_ok());
    assert!(Raw::decode(&mut &nested(40)[..]).is_err());
}

#[test]
fn encoders_refuse_oversize_messages() {
    let big = BftMessage::Vote(Vote {
        proposed_block_info: info(),
        signature: vec![0u8; 2000].into(),
        gap_number: 1,
    });
    assert!(matches!(
        eth::encode(Framing::Bare, &Message::Bft(big)),
        Err(eth::WireError::TooLarge {
            code: codes::VOTE,
            max: 1024,
            ..
        })
    ));
}

/// discv4 datagrams from geth's `v4wire.Encode` with `UseXDCPing` (signature included): ours
/// decode to the same packet, recover the same sender, and encode back to the same bytes.
#[test]
fn discv4_packets_equal_oracle_bytes() {
    use xone_net::discv4::{self, Endpoint, Packet, WireNode};
    let key = secp256k1::SecretKey::from_slice(&vec_bytes("discv4_key")).unwrap();
    let sender = B512::from_slice(&vec_bytes("discv4_pubkey"));
    assert_eq!(xone_net::rlpx::peer_id(&key), sender);
    let ep = |ip: &[u8], udp, tcp| Endpoint {
        ip: Bytes::copy_from_slice(ip),
        udp,
        tcp,
    };
    let ping = Packet::Ping {
        version: 4,
        from: ep(&[10, 0, 0, 1], 30303, 30303),
        to: ep(&[10, 0, 0, 2], 30304, 30305),
        expiration: 1_700_000_020,
    };
    let ping_bytes = vec_bytes("discv4_ping_xdc");
    assert_eq!(ping_bytes[32 + 65], 5, "the XDC ping is packet type 5");
    let ping_hash = B256::from_slice(&ping_bytes[..32]);
    let mut v6 = [0u8; 16];
    v6[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
    v6[15] = 1;
    let node = |ip: &[u8], udp, tcp| WireNode {
        ip: Bytes::copy_from_slice(ip),
        udp,
        tcp,
        id: sender,
    };
    let cases = [
        ("discv4_ping_xdc", ping.clone()),
        (
            "discv4_pong",
            Packet::Pong {
                to: ep(&[10, 0, 0, 1], 30303, 30303),
                reply_tok: Bytes::copy_from_slice(ping_hash.as_slice()),
                expiration: 1_700_000_020,
            },
        ),
        (
            "discv4_findnode",
            Packet::FindNode {
                target: sender,
                expiration: 1_700_000_020,
            },
        ),
        (
            "discv4_neighbors",
            Packet::Neighbors {
                nodes: vec![node(&[10, 0, 0, 3], 30303, 30303), node(&v6, 30304, 30305)],
                expiration: 1_700_000_020,
            },
        ),
    ];
    for (name, want) in cases {
        let bytes = vec_bytes(name);
        let (got, from, hash) = discv4::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (&got, from, hash.as_slice()),
            (&want, sender, &bytes[..32]),
            "{name}"
        );
        let (enc, h) = discv4::encode(&key, &want);
        assert_eq!(enc, bytes, "{name}: encode differs from geth");
        assert_eq!(h.as_slice(), &bytes[..32]);
    }
    // a one-bit change anywhere is rejected: hash, signature or payload
    for i in [0, 40, 96, 97, 100] {
        let mut b = ping_bytes.clone();
        b[i] ^= 1;
        assert!(discv4::decode(&b).is_err(), "flip at {i}");
    }
}

/// #119: geth-XDC announces `BlockRangeUpdate` (0x11, `[earliest, latest, latestHash]`, no request
/// id) on eth/101 (`xdpos2rid`, `eth/protocols/eth/handler.go:306`). It decodes and re-encodes as is.
#[test]
fn block_range_update_is_accepted_on_eth101() {
    use alloy_rlp::Encodable;
    let mut p = Vec::new();
    0u64.encode(&mut p);
    776_545u64.encode(&mut p);
    h(3).encode(&mut p);
    let mut body = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: p.len(),
    }
    .encode(&mut body);
    body.extend(p);
    let m = v101::decode(codes::BLOCK_RANGE_UPDATE, &body).unwrap();
    assert!(matches!(
        m,
        Message::Opaque {
            code: codes::BLOCK_RANGE_UPDATE,
            ..
        }
    ));
    assert_eq!(v101::encode(&m).unwrap(), (codes::BLOCK_RANGE_UPDATE, body));
}
