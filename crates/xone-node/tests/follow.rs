#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Twenty blocks of the committed Apothem replay vectors, delivered by a fake peer over an in-memory
//! pipe to the net service, followed by the chain service through `Chain::insert`: the head, the
//! roots recomputed from the node's own state and receipts, and an RPC answer over HTTP.

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::U256;
use alloy_trie::root::ordered_trie_root_encoded;
use common::{call, free_port, scratch, vectors};
use serde_json::json;
use tokio::runtime::Runtime;
use xone_net::{
    eth::{Block as WireBlock, BlockBody, GetHeaders, Message, Origin},
    peers::{Config as PeersConfig, Event, Peers},
};
use xone_node::{datadir, services, Config};
use xone_state::trie::flat_root;

const STAMP: &str = "xOne/v0.0.0-test/linux-x86_64/rustc1";

/// A peer that holds `blocks` and answers header and body requests.
fn serve(
    rt: &Runtime,
    blocks: Arc<Vec<WireBlock>>,
    peers: Peers,
    mut rx: tokio::sync::mpsc::Receiver<Event>,
) {
    rt.spawn(async move {
        while let Some(ev) = rx.recv().await {
            let Event::Message { peer, msg } = ev else {
                continue;
            };
            let reply = match msg {
                Message::GetBlockHeaders {
                    id,
                    query:
                        GetHeaders {
                            origin,
                            amount,
                            skip,
                            reverse,
                        },
                } => {
                    let mut at = match origin {
                        Origin::Number(n) => Some(n),
                        Origin::Hash(h) => blocks
                            .iter()
                            .position(|b| b.header.hash() == h)
                            .map(|i| i as u64),
                    };
                    let mut headers = vec![];
                    while let Some(n) = at.filter(|_| (headers.len() as u64) < amount) {
                        let Some(b) = blocks.get(n as usize) else {
                            break;
                        };
                        headers.push(b.header.clone());
                        let step = skip + 1;
                        at = if reverse {
                            n.checked_sub(step)
                        } else {
                            Some(n + step)
                        };
                    }
                    Message::BlockHeaders { id, headers }
                }
                Message::GetBlockBodies { id, hashes } => Message::BlockBodies {
                    id,
                    bodies: hashes
                        .iter()
                        .filter_map(|h| blocks.iter().find(|b| b.header.hash() == *h))
                        .map(|b| BlockBody {
                            transactions: b.transactions.clone(),
                            uncles: b.uncles.clone(),
                        })
                        .collect(),
                },
                _ => continue,
            };
            let _ = peers.send(&peer, reply);
        }
    });
}

#[test]
fn twenty_vector_blocks_through_net_and_chain_services() {
    let blocks = Arc::new(vectors(20));
    let dir = scratch("follow");
    let port = free_port();
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.http.enabled = true;
    cfg.http.port = port;
    cfg.http.api = vec!["eth".into(), "web3".into(), "XDPoS".into()];
    cfg.ipc.disabled = true;
    let (marker, net, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, net, &genesis, STAMP).unwrap();
    built.node.start().unwrap();

    // the fake peer: its own identity, a TD above ours, the pipe to our net service
    let rt = Runtime::new().unwrap();
    let mut pc = PeersConfig::new("fake", 51, built.core.genesis);
    pc.head = blocks[20].header.hash();
    pc.td = U256::from(1_000_000u64);
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let (fake, rx) = Peers::new(pc, key);
    serve(&rt, blocks.clone(), fake.clone(), rx);
    let (a, b) = tokio::io::duplex(1 << 20);
    let ours = built.net.peers.clone();
    let our_id = built.net.id;
    let (r1, r2) =
        rt.block_on(async { tokio::join!(ours.attach(a, None), fake.attach(b, Some(our_id))) });
    r1.unwrap();
    r2.unwrap();

    let t = Instant::now();
    while built.core.head().0 < 20 {
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "head {:?}",
            built.core.head()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let want = blocks[20].header.hash();
    assert_eq!(built.core.head(), (20, want));

    // the roots, recomputed here from what the node holds, equal the header's
    let h = built.core.hist.read();
    let tip = h.tip();
    assert_eq!(
        tip.header.receipt_root,
        ordered_trie_root_encoded(&tip.receipts)
    );
    assert_eq!(
        tip.header.tx_root,
        ordered_trie_root_encoded(&tip.body.transactions)
    );
    let state = built.core.chain.with_store(|s| flat_root(s.head_flat()));
    assert_eq!(state, tip.header.state_root);
    let txs: usize = h.blocks.iter().map(|b| b.body.transactions.len()).sum();
    drop(h);
    println!("20 blocks imported, {txs} transactions, state root {state}");

    // an RPC answer
    assert_eq!(call(port, "eth_blockNumber", json!([])), json!("0x14"));
    let b20 = call(port, "eth_getBlockByNumber", json!(["0x14", false]));
    assert_eq!(b20["hash"], json!(want));
    assert_eq!(b20["stateRoot"], json!(blocks[20].header.state_root));
    assert_eq!(call(port, "web3_clientVersion", json!([])), json!(STAMP));

    let t = Instant::now();
    built.node.stop().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    // the stop wrote a checkpoint of block 20
    assert!(dir.join("xone/state/ckpt-00000020.json.gz").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The discv4 loop bonds with a boot node: its first datagram is a Ping from our node id.
#[test]
fn discovery_pings_the_boot_node() {
    use xone_net::discv4::{decode, Packet};
    let boot = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    boot.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let boot_id = xone_net::rlpx::peer_id(&secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap());
    let dir = scratch("disc");
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.discovery_port = Some(0);
    cfg.p2p.bootnodes = vec![format!(
        "enode://{}@127.0.0.1:1?discport={}",
        alloy_primitives::hex::encode(boot_id),
        boot.local_addr().unwrap().port()
    )];
    cfg.ipc.disabled = true;
    // loopback listener for the test only: nothing but this socket is addressed
    let (marker, net, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, net, &genesis, STAMP).unwrap();
    built.node.start().unwrap();
    let mut buf = [0u8; 2048];
    let (n, _) = boot.recv_from(&mut buf).unwrap();
    let (packet, from, _) = decode(&buf[..n]).unwrap();
    assert!(matches!(packet, Packet::Ping { .. }));
    assert_eq!(from, built.net.id);
    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
