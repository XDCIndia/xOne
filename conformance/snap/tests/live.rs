#![allow(clippy::disallowed_methods)]
//! Live interop, loopback only. Ignored by default. Start a geth-XDC with a private chain
//! (`crates/xone-net/tests/local-geth/run.sh`, `MODE=prepare` once, then `MODE=serve`), then
//!
//! ```text
//! XONE_GETH_ENODE=$(cat $DATADIR/enode.txt) XONE_GETH_GENESIS=$(cat $DATADIR/genesis.txt) \
//!   cargo test --locked -p xone-snap-conformance --test live -- --ignored --nocapture
//! ```
//!
//! xone instance A snap-syncs the head state of the geth chain, checks the reconstructed root
//! against the head header's `stateRoot`, and serves it over RLPx to instance B, which syncs it
//! again and must reach the same root.

use alloy_primitives::{keccak256, Address, Bytes, B256, KECCAK256_EMPTY};
use reth_network_peers::NodeRecord;
use std::time::Instant;
use tokio::net::TcpListener;
use xone_net::rlpx::{peer_id, Conn};
use xone_snap::{
    client::{sync, Config, PeerSource, StateStore},
    codec::{GetAccountRange, GetByteCodes, GetStorageRanges, GetTrieNodes, Message},
    server::{Limits, Server},
};
use xone_snap_conformance::{dial, has_snap, header_by_hash, hello, key, serve, SnapPeer};

const NETWORK_ID: u64 = 5551;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}, see the module docs"))
}

fn summary(label: &str, s: &StateStore, p: &SnapPeer) {
    let slots: usize = s.storage.values().map(|x| x.len()).sum();
    let wide = s.accounts.values().filter(|a| a.balance.is_wide()).count();
    println!(
        "{label}: {} accounts ({wide} wide), {} storage tries, {slots} slots, {} codes; {} snap exchanges, {} eth frames dropped; root {}",
        s.accounts.len(), s.storage.len(), s.codes.len(), p.exchanges, p.eth_dropped,
        s.progress.verified.map_or("UNVERIFIED".into(), |r| format!("{r:#x}")),
    );
}

/// The same requests to geth and to our server over the same state: the answers must be equal
/// message for message, proofs and node order included.
fn same_answers_as_geth(store: &StateStore, geth: &mut SnapPeer) {
    let ours = Server::new(store.snapshot(), Limits::default());
    let root = store.progress.root;
    let max = B256::repeat_byte(0xff);
    let big = keccak256(Address::with_last_byte(0x88));
    let some_slot = *store.storage[&big].keys().nth(400).unwrap();
    let with_storage: Vec<B256> = store.storage.keys().copied().collect();
    let codes: Vec<B256> = store
        .codes
        .keys()
        .copied()
        .chain([KECCAK256_EMPTY])
        .collect();
    let mid = *store.accounts.keys().nth(store.accounts.len() / 2).unwrap();
    let node_paths = vec![
        vec![Bytes::from_static(&[0x00])],
        vec![Bytes::from_static(&[0x10])],
        vec![Bytes::from_static(&[0x01, 0x23])],
        vec![big.0.to_vec().into(), Bytes::from_static(&[0x00])],
        vec![
            big.0.to_vec().into(),
            Bytes::from_static(&[0x00]),
            Bytes::from_static(&[0x1a]),
        ],
    ];
    let reqs = vec![
        Message::GetAccountRange(GetAccountRange {
            id: 1,
            root,
            origin: B256::ZERO,
            limit: max,
            bytes: 600,
        }),
        Message::GetAccountRange(GetAccountRange {
            id: 2,
            root,
            origin: mid,
            limit: max,
            bytes: 4096,
        }),
        Message::GetAccountRange(GetAccountRange {
            id: 3,
            root,
            origin: B256::ZERO,
            limit: max,
            bytes: 1 << 20,
        }),
        Message::GetAccountRange(GetAccountRange {
            id: 4,
            root,
            origin: max,
            limit: max,
            bytes: 4096,
        }),
        Message::GetStorageRanges(GetStorageRanges {
            id: 5,
            root,
            accounts: vec![big],
            origin: Bytes::new(),
            limit: Bytes::new(),
            bytes: 4096,
        }),
        Message::GetStorageRanges(GetStorageRanges {
            id: 6,
            root,
            accounts: vec![big],
            origin: some_slot.0.to_vec().into(),
            limit: Bytes::new(),
            bytes: 2048,
        }),
        Message::GetStorageRanges(GetStorageRanges {
            id: 7,
            root,
            accounts: with_storage,
            origin: Bytes::new(),
            limit: Bytes::new(),
            bytes: 1 << 20,
        }),
        Message::GetByteCodes(GetByteCodes {
            id: 8,
            hashes: codes,
            bytes: 1 << 20,
        }),
        Message::GetTrieNodes(GetTrieNodes {
            id: 9,
            root,
            paths: node_paths,
            bytes: 1 << 20,
        }),
    ];
    for req in reqs {
        let want = geth.exchange(req.clone()).expect("geth answers");
        let got = ours.respond(&req).expect("we answer");
        assert_eq!(got, want, "request {} differs from geth", req.id());
        println!(
            "same answer as geth for request {}: {} bytes",
            req.id(),
            want.encode().len()
        );
    }
}

#[tokio::test]
#[ignore = "needs a local geth-XDC node, see tests/live.rs"]
async fn snap_sync_from_geth_then_serve_to_a_second_xone() {
    let node: NodeRecord = env("XONE_GETH_ENODE").parse().expect("enode url");
    let genesis: B256 = env("XONE_GETH_GENESIS").parse().expect("genesis hash");
    // small requests: many ranges, geth cuts storage tries and proves each cut
    let cfg = Config {
        request_bytes: 4096,
        ..Config::default()
    };

    // ---- A: geth -> xone
    let (mut conn, status) = dial(
        (node.address, node.tcp_port).into(),
        node.id,
        key(),
        NETWORK_ID,
        genesis,
    )
    .await
    .expect("dial and handshake");
    println!(
        "geth Hello: {:?} (eth/{}), caps {:?}",
        conn.hello.name,
        conn.eth_version,
        conn.hello
            .caps
            .iter()
            .map(|c| format!("{}/{}", c.name, c.version))
            .collect::<Vec<_>>()
    );
    assert!(has_snap(&conn), "geth must advertise snap/1");
    let head = header_by_hash(&mut conn, status.head)
        .await
        .expect("head header");
    println!(
        "geth head: block {} {} stateRoot {}",
        head.number, status.head, head.state_root
    );
    assert!(head.number > 0, "prepare the chain first");
    let handle = tokio::runtime::Handle::current();
    let mut a = SnapPeer::new(handle.clone(), conn);
    let root = head.state_root;
    let (store_a, a) = tokio::task::spawn_blocking(move || {
        let mut store = StateStore::new(root);
        sync(&mut store, &mut a, &cfg).expect("snap sync from geth");
        same_answers_as_geth(&store, &mut a);
        (store, a)
    })
    .await
    .unwrap();
    summary("A (from geth)", &store_a, &a);
    assert_eq!(
        store_a.progress.verified,
        Some(head.state_root),
        "reconstructed root == header stateRoot"
    );

    // ---- B: A -> second xone, over RLPx on loopback
    let key_a = key();
    let id_a = peer_id(&key_a);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut server = Server::new(store_a.snapshot(), Limits::default());
    assert_eq!(server.snapshot().root(), head.state_root);
    let serving = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut conn = Conn::accept(tcp, key_a, &hello()).await.unwrap();
        assert!(has_snap(&conn));
        let n = serve(&mut conn, &mut server, Instant::now()).await;
        (n, server)
    });
    let (conn_b, _) = {
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let c = Conn::connect(tcp, key(), id_a, &hello()).await.unwrap();
        (c, ())
    };
    assert!(has_snap(&conn_b));
    let mut b = SnapPeer::new(handle, conn_b);
    let cfg = Config {
        request_bytes: 4096,
        ..Config::default()
    };
    let (store_b, b) = tokio::task::spawn_blocking(move || {
        let mut store = StateStore::new(root);
        sync(&mut store, &mut b, &cfg).expect("snap sync from xone");
        (store, b)
    })
    .await
    .unwrap();
    summary("B (from A)", &store_b, &b);
    assert_eq!(store_b.progress.verified, Some(head.state_root));
    assert_eq!(store_b.accounts, store_a.accounts);
    assert_eq!(store_b.storage, store_a.storage);
    assert_eq!(store_b.codes, store_a.codes);
    drop(b);
    let (served, _) = serving.await.unwrap();
    println!(
        "A served {served} snap requests to B; B's state equals A's, root {}",
        head.state_root
    );
}
