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
    rx: tokio::sync::mpsc::Receiver<Event>,
) {
    serve_as(rt, blocks, peers, rx, Duration::ZERO, Instant::now(), false);
}

/// [`serve`], with each answer sent `delay` late, and before `until` every header answer is
/// empty (a peer that is behind) or, with `echo`, the origin alone (a peer still importing the head
/// it announced).
fn serve_as(
    rt: &Runtime,
    blocks: Arc<Vec<WireBlock>>,
    peers: Peers,
    mut rx: tokio::sync::mpsc::Receiver<Event>,
    delay: Duration,
    until: Instant,
    echo: bool,
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
                    if Instant::now() < until {
                        headers.truncate(usize::from(echo));
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
                            withdrawals: None,
                        })
                        .collect(),
                },
                _ => continue,
            };
            let p = peers.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = p.send(&peer, reply);
            });
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
    let (marker, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, spec, &genesis, STAMP).unwrap();
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
    let (marker, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, spec, &genesis, STAMP).unwrap();
    built.node.start().unwrap();
    let mut buf = [0u8; 2048];
    let (n, _) = boot.recv_from(&mut buf).unwrap();
    let (packet, from, _) = decode(&buf[..n]).unwrap();
    // geth-XDC p2p/discover/v4_udp.go:269: expiration is Unix seconds now + 20
    let Packet::Ping { expiration, .. } = packet else {
        panic!("not a Ping")
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        (now..=now + 30).contains(&expiration),
        "expiration {expiration} vs now {now}"
    );
    assert_eq!(from, built.net.id);
    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A peer that advertises a head it cannot serve, and answers every poll with only our own head, is
/// not proof that we are caught up: `eth_syncing` stays an object.
#[test]
fn an_echo_only_peer_does_not_confirm_caught_up() {
    let blocks = Arc::new(vectors(20));
    let dir = scratch("echo");
    let port = free_port();
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.http.enabled = true;
    cfg.http.port = port;
    cfg.http.api = vec!["eth".into()];
    cfg.ipc.disabled = true;
    let (marker, net, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, net, &genesis, STAMP).unwrap();
    built.node.start().unwrap();

    let rt = Runtime::new().unwrap();
    let mut pc = PeersConfig::new("fake", 51, built.core.genesis);
    pc.head = blocks[20].header.hash();
    pc.td = U256::from(1_000_000u64);
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let (fake, rx) = Peers::new(pc, key);
    // it holds only the genesis block: every header poll is answered with our own head alone
    serve(&rt, Arc::new(blocks[..1].to_vec()), fake.clone(), rx);
    let (a, b) = tokio::io::duplex(1 << 20);
    let ours = built.net.peers.clone();
    let our_id = built.net.id;
    let (r1, r2) =
        rt.block_on(async { tokio::join!(ours.attach(a, None), fake.attach(b, Some(our_id))) });
    r1.unwrap();
    r2.unwrap();

    // several one-second polls
    std::thread::sleep(Duration::from_secs(4));
    assert_ne!(call(port, "eth_syncing", json!([])), json!(false));
    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Starts an Apothem node with HTTP and connects it to a fake peer that holds `blocks` and
/// advertises the last of them, served by [`serve_as`].
fn with_fake_peer(
    tag: &str,
    blocks: &Arc<Vec<WireBlock>>,
    delay: Duration,
    until: Instant,
    echo: bool,
) -> (
    services::Built,
    std::path::PathBuf,
    Runtime,
    Peers,
    xone_net::rlpx::PeerId,
    tokio::sync::mpsc::UnboundedReceiver<Message>,
) {
    let dir = scratch(tag);
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.ipc.disabled = true;
    let (marker, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, spec, &genesis, STAMP).unwrap();
    built.node.start().unwrap();
    let rt = Runtime::new().unwrap();
    let mut pc = PeersConfig::new("fake", 51, built.core.genesis);
    pc.head = blocks[blocks.len() - 1].header.hash();
    pc.td = U256::from(1_000_000u64);
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let (fake, mut rx) = Peers::new(pc, key);
    // the answers of our node to the fake peer's own requests are tapped; the rest goes on to `serve_as`
    let (tap, replies) = tokio::sync::mpsc::unbounded_channel();
    let (fwd, rx2) = tokio::sync::mpsc::channel(1024);
    rt.spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::Message {
                    msg: m @ (Message::BlockBodies { .. } | Message::Receipts { .. }),
                    ..
                } => {
                    let _ = tap.send(m);
                }
                ev => {
                    let _ = fwd.send(ev).await;
                }
            }
        }
    });
    serve_as(&rt, blocks.clone(), fake.clone(), rx2, delay, until, echo);
    let (a, b) = tokio::io::duplex(1 << 20);
    let ours = built.net.peers.clone();
    let our_id = built.net.id;
    let (r1, r2) =
        rt.block_on(async { tokio::join!(ours.attach(a, None), fake.attach(b, Some(our_id))) });
    r1.unwrap();
    r2.unwrap();
    (built, dir, rt, fake, our_id, replies)
}

fn wait_head(built: &services::Built, n: u64, within: Duration) {
    let t = Instant::now();
    while built.core.head().0 < n {
        assert!(
            t.elapsed() < within,
            "head {:?} after {:?}, want {n}",
            built.core.head(),
            t.elapsed()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A `NewBlock` whose parent is our head is imported at once, also while a request is in flight
/// (geth-XDC `eth/handler_eth.go:205-296`). The late answer to a header poll, which starts at the head
/// the poll was sent from, is not taken for a peer that does not know our head, and a late body
/// answer skips the blocks a `NewBlock` delivered meanwhile.
#[test]
fn new_block_is_imported_while_a_poll_is_in_flight() {
    let blocks = Arc::new(vectors(20));
    let t0 = Instant::now();
    let (mut built, dir, rt, fake, our_id, _) = with_fake_peer(
        "nbpoll",
        &blocks,
        Duration::from_secs(3),
        Instant::now(),
        false,
    );
    let announce = |r: std::ops::RangeInclusive<usize>| {
        for b in &blocks[r] {
            let nb = xone_net::eth::NewBlock {
                block: b.clone(),
                td: U256::from(1_000_000u64),
            };
            fake.send(&our_id, Message::NewBlock(Box::new(nb))).unwrap();
        }
    };
    // every answer takes three seconds: the headers of the poll sent at genesis arrive at about
    // 3 s, the bodies of 11 to 20 at about 6 s; the blocks 1 to 10 come as NewBlock at once
    announce(1..=10);
    wait_head(&built, 10, Duration::from_millis(2500));
    // 11 and 12 come as NewBlock while the bodies are in flight
    std::thread::sleep(Duration::from_millis(4500).saturating_sub(t0.elapsed()));
    announce(11..=12);
    wait_head(&built, 12, Duration::from_millis(1000));
    wait_head(&built, 20, Duration::from_secs(15));
    assert_eq!(built.core.head().1, blocks[20].header.hash());
    built.node.stop().unwrap();
    drop(rt);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A peer that answers polls with nothing (behind us) or with our head alone while it advertises
/// more (still importing what it announced) is asked again within seconds, not set aside for a
/// minute: here it is the only peer, and it serves its head after 1.5 s.
#[test]
fn a_peer_that_is_not_yet_there_is_asked_again_soon() {
    for (tag, echo) in [("notyet-empty", false), ("notyet-echo", true)] {
        let blocks = Arc::new(vectors(20));
        let until = Instant::now() + Duration::from_millis(1500);
        let (mut built, dir, rt, _fake, _, _) =
            with_fake_peer(tag, &blocks, Duration::ZERO, until, echo);
        wait_head(&built, 20, Duration::from_secs(15));
        built.node.stop().unwrap();
        drop(rt);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The eth/101 request deadline of the follower (`chain.rs` `REQUEST_TIMEOUT`): a peer that answers later
/// is set aside for 60 s.
const DEADLINE: Duration = Duration::from_secs(10);

/// Asks `our_id` for the bodies and then the receipts of `hashes` and returns both answers with the time
/// each took.
fn ask_old(
    rt: &Runtime,
    fake: &Peers,
    our_id: &xone_net::rlpx::PeerId,
    replies: &mut tokio::sync::mpsc::UnboundedReceiver<Message>,
    hashes: &[alloy_primitives::B256],
    id: u64,
) -> (
    (Vec<BlockBody>, Duration),
    (Vec<xone_net::eth::Raw>, Duration),
) {
    let mut one = |req: Message| {
        let t = Instant::now();
        fake.send(our_id, req).unwrap();
        let m = rt
            .block_on(async { tokio::time::timeout(DEADLINE, replies.recv()).await })
            .expect("no answer within the deadline")
            .expect("the tap closed");
        (m, t.elapsed())
    };
    let (m, tb) = one(Message::GetBlockBodies {
        id: Some(id),
        hashes: hashes.to_vec(),
    });
    let Message::BlockBodies { id: got, bodies } = m else {
        panic!("not BlockBodies: {m:?}")
    };
    assert_eq!(got, Some(id));
    let (m, tr) = one(Message::GetReceipts {
        id: Some(id + 1),
        first_index: 0,
        hashes: hashes.to_vec(),
    });
    let Message::Receipts { per_block, .. } = m else {
        panic!("not Receipts: {m:?}")
    };
    ((bodies, tb), (per_block, tr))
}

/// XB-36: 128 old bodies and receipts are answered within the deadline while imports continue, and
/// also while the import lock and the store's write lock are held (a long commit): the serving path
/// reads the segment files and one MDBX read transaction only. A range the peer does not hold is
/// answered with an empty list at once (geth-XDC `eth/protocols/eth/handlers.go:260-277`).
#[test]
fn old_bodies_are_served_within_the_deadline_without_the_import_lock() {
    let blocks = Arc::new(vectors(400));
    // every answer of the fake peer is slow, so that imports are still running during the first request
    let (mut built, dir, rt, fake, our_id, mut replies) = with_fake_peer(
        "oldbodies",
        &blocks,
        Duration::from_millis(400),
        Instant::now(),
        false,
    );
    wait_head(&built, 300, Duration::from_secs(60));
    // blocks 1 to 128 are below the memory window of the node: they are read from the store
    let old = &blocks[1..=128];
    let hashes: Vec<_> = old.iter().map(|b| b.header.hash()).collect();
    let check = |bodies: &[BlockBody], receipts: &[xone_net::eth::Raw]| {
        assert_eq!(bodies.len(), 128);
        assert_eq!(receipts.len(), 128);
        for (b, w) in old.iter().zip(bodies) {
            assert_eq!(w.transactions, b.transactions, "block {}", b.header.number);
        }
    };

    // 1: while the node is importing
    assert!(built.core.head().0 < 400, "imports have to continue");
    let ((bodies, tb), (receipts, tr)) = ask_old(&rt, &fake, &our_id, &mut replies, &hashes, 1000);
    println!("imports running: 128 bodies in {tb:?}, 128 receipts in {tr:?}");
    check(&bodies, &receipts);
    assert!(tb < DEADLINE && tr < DEADLINE);

    // 2: while a writer holds the import lock and the store for 4 s
    wait_head(&built, 400, Duration::from_secs(60));
    let core = built.core.clone();
    let (held, release) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        core.chain.with_store_mut(|_| {
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(4));
        });
    });
    release.recv().unwrap();
    let ((bodies, tb), (receipts, tr)) = ask_old(&rt, &fake, &our_id, &mut replies, &hashes, 2000);
    println!("import lock held: 128 bodies in {tb:?}, 128 receipts in {tr:?}");
    check(&bodies, &receipts);
    assert!(
        tb < Duration::from_secs(2) && tr < Duration::from_secs(2),
        "the answer waited for the lock: {tb:?}, {tr:?}"
    );
    writer.join().unwrap();

    // 3: a range the peer does not hold: an empty list at once, not a silence
    let unknown: Vec<_> = (0..128u8)
        .map(alloy_primitives::B256::repeat_byte)
        .collect();
    let ((bodies, tb), (receipts, tr)) = ask_old(&rt, &fake, &our_id, &mut replies, &unknown, 3000);
    assert!(bodies.is_empty() && receipts.is_empty());
    assert!(tb < Duration::from_secs(1) && tr < Duration::from_secs(1));
    // a known block first and an unknown one after it: the answer stops there, nothing is skipped
    let mixed = [hashes[0], unknown[0], hashes[1]];
    let ((bodies, _), (receipts, _)) = ask_old(&rt, &fake, &our_id, &mut replies, &mixed, 4000);
    assert_eq!((bodies.len(), receipts.len()), (1, 1));
    assert_eq!(bodies[0].transactions, old[0].transactions);

    built.node.stop().unwrap();
    drop(rt);
    let _ = std::fs::remove_dir_all(&dir);
}

/// XB-36, the live measurement (ignored: it needs a node). A fresh client, with a throwaway identity
/// and no datadir, asks the node at `XONE_LIVE_ADDR` (host:port, TCP and UDP) for the headers 897 to 1024
/// of net2040 and then, in order, for their bodies and their receipts, one 128-block batch each, and
/// prints the time of every answer (`XONE_LIVE_ROUNDS` body and receipt rounds, default 1). The node id comes from the Pong of a discv4 Ping to the same port.
///
/// ```text
/// XONE_LIVE_ADDR=127.0.0.1:30303 cargo test --release --locked -p xone-node --test follow \
///   live_old_bodies -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a live net2040 node, see the doc comment"]
#[allow(clippy::disallowed_methods)] // the address of the node under test comes from the environment
fn live_old_bodies() {
    use xone_net::discv4::{decode, encode, Endpoint, Packet};
    let addr: std::net::SocketAddr = std::env::var("XONE_LIVE_ADDR")
        .expect("set XONE_LIVE_ADDR")
        .parse()
        .expect("host:port");
    let mut cfg = Config::default();
    cfg.set("net2040", "true").unwrap();
    cfg.datadir = scratch("live");
    let (_, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let genesis_hash = genesis.hash(&spec).unwrap();
    let _ = std::fs::remove_dir_all(&cfg.datadir);

    // the node id: the signer of the Pong
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let key = secp256k1::SecretKey::from_slice(&[11u8; 32]).unwrap();
    let ep = |a: std::net::SocketAddr| Endpoint::new(a, a.port());
    let expiration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 20;
    let (ping, _) = encode(
        &key,
        &Packet::Ping {
            version: 4,
            from: ep(sock.local_addr().unwrap()),
            to: ep(addr),
            expiration,
        },
    );
    sock.send_to(&ping, addr).unwrap();
    let mut buf = [0u8; 2048];
    let node_id = loop {
        let (n, _) = sock.recv_from(&mut buf).expect("no Pong from the node");
        if let Ok((Packet::Pong { .. }, id, _)) = decode(&buf[..n]) {
            break id;
        }
    };

    let rt = Runtime::new().unwrap();
    let mut pc = PeersConfig::new("xone-xb36-probe", spec.chain_id, genesis_hash);
    pc.td = U256::from(1u64);
    let (me, mut rx) = Peers::new(pc, secp256k1::SecretKey::from_slice(&[12u8; 32]).unwrap());
    let version = rt.block_on(async {
        me.dial(addr, node_id).await.expect("dial and handshake");
        loop {
            if let Some(Event::Connected { version, .. }) = rx.recv().await {
                break version;
            }
        }
    });
    let mut next_id = 0u64;
    let mut ask = |req: Box<dyn Fn(Option<u64>) -> Message>| {
        next_id += 1;
        let id = (version == 101).then_some(next_id);
        let t = Instant::now();
        me.send(&node_id, req(id)).unwrap();
        let m = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(25), async {
                loop {
                    match rx.recv().await {
                        Some(Event::Message {
                            msg:
                                m @ (Message::BlockHeaders { .. }
                                | Message::BlockBodies { .. }
                                | Message::Receipts { .. }),
                            ..
                        }) => break m,
                        Some(e) => println!("event: {e:?}"),
                        None => panic!("closed"),
                    }
                }
            })
            .await
        });
        (m.expect("no answer within 25 s"), t.elapsed())
    };
    let (m, t) = ask(Box::new(|id| Message::GetBlockHeaders {
        id,
        query: GetHeaders {
            origin: Origin::Number(897),
            amount: 128,
            skip: 0,
            reverse: false,
        },
    }));
    let Message::BlockHeaders { headers, .. } = m else {
        panic!("not headers")
    };
    println!("eth/{version}: {} headers 897.. in {t:?}", headers.len());
    assert_eq!(headers.len(), 128);
    let hashes: Vec<_> = headers.iter().map(|h| h.hash()).collect();
    let rounds: u32 = std::env::var("XONE_LIVE_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    for round in 1..=rounds {
        // the whole range: each request names the hashes that are still missing, and the peer answers
        // the first blocks up to its soft limit (geth does the same), so a batch is several requests
        for kind in ["bodies", "receipts"] {
            let (mut got, mut txs, mut reqs) = (0, 0, 0);
            let (mut worst, t0) = (Duration::ZERO, Instant::now());
            while got < hashes.len() {
                let hs = hashes[got..].to_vec();
                let bodies = kind == "bodies";
                let (m, t) = ask(Box::new(move |id| {
                    if bodies {
                        Message::GetBlockBodies {
                            id,
                            hashes: hs.clone(),
                        }
                    } else {
                        Message::GetReceipts {
                            id,
                            first_index: 0,
                            hashes: hs.clone(),
                        }
                    }
                }));
                let n = match m {
                    Message::BlockBodies { bodies, .. } => {
                        txs += bodies.iter().map(|b| b.transactions.len()).sum::<usize>();
                        bodies.len()
                    }
                    Message::Receipts { per_block, .. } => per_block.len(),
                    _ => panic!("not an answer to {kind}"),
                };
                assert!(n > 0, "an empty answer for a retained block");
                (got, reqs, worst) = (got + n, reqs + 1, worst.max(t));
            }
            println!(
                "round {round}: {kind} of 128 blocks ({txs} txs): {reqs} requests, slowest answer \
                 {worst:?}, all in {:?} (deadline {DEADLINE:?})",
                t0.elapsed()
            );
            assert!(worst < DEADLINE);
        }
    }
}
