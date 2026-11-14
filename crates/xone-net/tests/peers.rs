#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Sessions over in-memory pipes: real ECIES, Hello and Status on both sides, fake peers that
//! can send any frame. Deterministic keys; no sockets.

use std::time::{Duration, Instant};

use alloy_primitives::{B256, U256};
use secp256k1::SecretKey;
use tokio::io::{duplex, DuplexStream};
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;
use xone_net::eth::{self, codes, BftMessage, Framing, GetHeaders, Message, Origin, Status62};
use xone_net::peers::{Config, Event, Peers};
use xone_net::rlpx::{peer_id, Cap, Conn, DisconnectReason, LocalHello, PeerId};
use xone_net::NetError;
use xone_primitives::{BlockInfo, Vote};

const NET: u64 = 5551;
const T: Duration = Duration::from_secs(10);

fn genesis() -> B256 {
    B256::repeat_byte(0x42)
}

fn key(i: u8) -> SecretKey {
    SecretKey::from_slice(&[i; 32]).unwrap()
}

fn cfg() -> Config {
    Config::new("xone-test", NET, genesis())
}

fn status(version: u64, network_id: u64) -> Vec<u8> {
    alloy_rlp::encode(Status62 {
        version: u32::try_from(version).unwrap(),
        network_id,
        td: U256::from(5),
        head: genesis(),
        genesis: genesis(),
    })
}

fn both() -> Vec<Cap> {
    vec![Cap::eth(101), Cap::eth(100)]
}

fn vote(round: u64) -> BftMessage {
    BftMessage::Vote(Vote {
        proposed_block_info: BlockInfo {
            hash: B256::repeat_byte(9),
            round,
            number: 10,
        },
        signature: vec![1; 65].into(),
        gap_number: 450,
    })
}

struct Fake {
    conn: Conn<DuplexStream>,
}

impl Fake {
    /// Next eth message of the node as `(code, body)`.
    async fn recv(&mut self) -> Result<(u64, Vec<u8>), NetError> {
        timeout(T, self.conn.recv())
            .await
            .expect("fake recv timed out")
    }

    /// Read until the node disconnects; the reason it gave.
    async fn until_disconnect(&mut self) -> DisconnectReason {
        loop {
            match self.recv().await {
                Err(NetError::Disconnect(r)) => return r,
                Err(e) => panic!("expected a Disconnect, got {e}"),
                Ok(_) => {}
            }
        }
    }

    async fn send(&mut self, code: u64, body: &[u8]) {
        self.conn.send(code, body).await.unwrap();
    }
}

async fn fake_connect(
    io: DuplexStream,
    k: SecretKey,
    node: PeerId,
    caps: Vec<Cap>,
    status: impl FnOnce(u64) -> Vec<u8>,
) -> Result<Fake, NetError> {
    let mut conn = Conn::connect(
        io,
        k,
        node,
        &LocalHello {
            name: "fake".into(),
            caps,
        },
    )
    .await?;
    let v = conn.eth_version;
    conn.send(codes::STATUS, &status(v)).await?;
    let (code, _) = conn.recv().await?;
    assert_eq!(code, codes::STATUS);
    Ok(Fake { conn })
}

/// The node accepts an inbound connection from a fake with key `k`.
async fn join_with(
    node: &Peers,
    k: SecretKey,
    caps: Vec<Cap>,
    buf: usize,
    st: impl FnOnce(u64) -> Vec<u8>,
) -> (Result<PeerId, NetError>, Result<Fake, NetError>) {
    let (a, b) = duplex(buf);
    tokio::join!(
        node.attach(a, None),
        fake_connect(b, k, node.id(), caps, st)
    )
}

async fn join(node: &Peers, k: SecretKey) -> Fake {
    let (r, f) = join_with(node, k, both(), 64 * 1024, |v| status(v, NET)).await;
    assert_eq!(r.unwrap(), peer_id(&k));
    f.unwrap()
}

async fn next_event(rx: &mut Receiver<Event>) -> Event {
    timeout(T, rx.recv())
        .await
        .expect("event timed out")
        .expect("event channel closed")
}

/// The next `Message` event, skipping Connected.
async fn next_msg(rx: &mut Receiver<Event>) -> (PeerId, Message) {
    loop {
        if let Event::Message { peer, msg } = next_event(rx).await {
            return (peer, msg);
        }
    }
}

async fn expect_disconnected(rx: &mut Receiver<Event>, who: PeerId) {
    loop {
        if matches!(next_event(rx).await, Event::Disconnected { peer } if peer == who) {
            return;
        }
    }
}

fn headers_request(f: Framing, id: u64) -> Vec<u8> {
    let msg = Message::GetBlockHeaders {
        id: (f == Framing::RequestId).then_some(id),
        query: GetHeaders {
            origin: Origin::Number(0),
            amount: 1,
            skip: 0,
            reverse: false,
        },
    };
    eth::encode(f, &msg).unwrap().1
}

/// `n` peers connected, `stalled` of them never read again. Returns how long `shutdown` took and
/// its report, after checking every healthy peer got a Quitting disconnect.
async fn shutdown_trial(
    n: u8,
    stalled: u8,
    mut c: Config,
) -> (Duration, xone_net::peers::ShutdownReport) {
    c.max_peers = usize::from(n);
    let (node, _events) = Peers::new(c, key(1));
    let mut healthy = Vec::new();
    for i in 0..n {
        let k = key(2 + i);
        if i < stalled {
            // a small pipe that nobody reads: after a few messages a write to this peer blocks
            let (r, f) = join_with(&node, k, both(), 1024, |v| status(v, NET)).await;
            r.unwrap();
            let f = f.unwrap();
            let id = peer_id(&k);
            for r in 0..40 {
                node.send(
                    &id,
                    Message::BlockHeaders {
                        id: Some(r),
                        headers: vec![],
                    },
                )
                .unwrap();
            }
            tokio::spawn(async move {
                let _keep = f;
                std::future::pending::<()>().await;
            });
        } else {
            let mut f = join(&node, k).await;
            healthy.push(tokio::spawn(async move { f.until_disconnect().await }));
        }
    }
    assert_eq!(node.peer_count(), usize::from(n));
    let t = Instant::now();
    let report = node.shutdown().await;
    let took = t.elapsed();
    assert_eq!(node.peer_count(), 0);
    for h in healthy {
        assert_eq!(h.await.unwrap(), DisconnectReason::Quitting);
    }
    // no new connection after shutdown
    let (a, _b) = duplex(1024);
    assert!(node.attach(a, None).await.is_err());
    (took, report)
}

#[tokio::test]
async fn shutdown_with_50_peers_closes_every_one_within_the_bound() {
    let (took, r) = shutdown_trial(50, 0, cfg()).await;
    assert_eq!((r.closed, r.aborted), (50, 0));
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[tokio::test]
async fn shutdown_aborts_peers_that_stopped_reading() {
    // Disconnect writes may take 5 s, but the grace time is 300 ms: stalled sessions are aborted.
    let mut c = cfg();
    c.disconnect_timeout = Duration::from_secs(5);
    c.shutdown_grace = Duration::from_millis(300);
    let (took, r) = shutdown_trial(50, 7, c).await;
    assert_eq!((r.closed, r.aborted), (43, 7));
    assert!(took < Duration::from_millis(900), "{took:?}");
}

/// The gate: 100 trials, p99 at or under 5 s. Ignored by default (about a minute in debug).
#[tokio::test]
#[ignore = "100 trials of 50 peers"]
async fn shutdown_100_trials_p99_under_5s() {
    let mut times = Vec::new();
    for _ in 0..100 {
        let (took, r) = shutdown_trial(50, 0, cfg()).await;
        assert_eq!(r.aborted, 0);
        times.push(took);
    }
    times.sort();
    let p99 = times[98];
    println!(
        "shutdown, 50 peers, 100 trials: p50 {:?} p99 {:?} max {:?}",
        times[49], p99, times[99]
    );
    assert!(p99 <= Duration::from_secs(5));
}

#[tokio::test]
async fn handshake_is_bounded_and_status_is_validated() {
    let mut c = cfg();
    c.handshake_timeout = Duration::from_millis(200);
    let (node, mut rx) = Peers::new(c, key(1));
    // a peer that opens the pipe and says nothing
    let (a, _silent) = duplex(1024);
    let t = Instant::now();
    assert!(matches!(
        node.attach(a, None).await,
        Err(NetError::Timeout("handshake"))
    ));
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(node.peer_count(), 0);

    // wrong network id and wrong genesis are refused, with a Disconnect to the peer
    let (r, f) = join_with(&node, key(2), both(), 64 * 1024, |v| status(v, 1)).await;
    assert!(matches!(
        r,
        Err(NetError::Status(eth::status62::StatusError::NetworkId {
            got: 1,
            ..
        }))
    ));
    assert!(
        f.is_err() || f.unwrap().until_disconnect().await == DisconnectReason::SubprotocolError
    );
    let (r, _) = join_with(&node, key(3), both(), 64 * 1024, |v| {
        alloy_rlp::encode(Status62 {
            version: u32::try_from(v).unwrap(),
            network_id: NET,
            td: U256::ZERO,
            head: B256::ZERO,
            genesis: B256::repeat_byte(1),
        })
    })
    .await;
    assert!(matches!(
        r,
        Err(NetError::Status(eth::status62::StatusError::Genesis { .. }))
    ));
    // no common eth version
    let (r, _) = join_with(&node, key(4), vec![Cap::eth(68)], 64 * 1024, |v| {
        status(v, NET)
    })
    .await;
    assert!(r.is_err());
    assert_eq!(node.peer_count(), 0);

    // xOneGo #684: a ForkID of any value does not keep a peer out
    let (r, f) = join_with(&node, key(5), both(), 64 * 1024, |v| {
        let mut b = status(v, NET);
        let fork_id = [
            0xcau8, 0x84, 0xde, 0xad, 0xbe, 0xef, 0x84, 0x00, 0x00, 0x00, 0x2a,
        ];
        b.extend_from_slice(&fork_id);
        b[1] += u8::try_from(fork_id.len()).unwrap();
        b
    })
    .await;
    assert_eq!(r.unwrap(), peer_id(&key(5)));
    drop(f.unwrap());
    assert!(matches!(
        next_event(&mut rx).await,
        Event::Connected {
            rank: 5,
            version: 101,
            inbound: true,
            ..
        }
    ));
}

#[tokio::test]
async fn both_framings_work_per_session() {
    let (node, mut rx) = Peers::new(cfg(), key(1));
    // a modern peer: eth/101, request ids
    let mut modern = join(&node, key(2)).await;
    modern
        .send(
            codes::GET_BLOCK_HEADERS,
            &headers_request(Framing::RequestId, 77),
        )
        .await;
    let (_, m) = next_msg(&mut rx).await;
    assert!(matches!(m, Message::GetBlockHeaders { id: Some(77), .. }));
    // a legacy peer: only eth/100, bare
    let (r, f) = join_with(&node, key(3), vec![Cap::eth(100)], 64 * 1024, |v| {
        status(v, NET)
    })
    .await;
    r.unwrap();
    let mut legacy = f.unwrap();
    legacy
        .send(codes::GET_BLOCK_HEADERS, &headers_request(Framing::Bare, 0))
        .await;
    let (p, m) = next_msg(&mut rx).await;
    assert_eq!(p, peer_id(&key(3)));
    assert!(matches!(m, Message::GetBlockHeaders { id: None, .. }));
    // replies use each peer's framing
    let reply = |id| Message::BlockHeaders {
        id,
        headers: vec![],
    };
    node.send(&peer_id(&key(2)), reply(Some(77))).unwrap();
    node.send(&peer_id(&key(3)), reply(None)).unwrap();
    let (c, body) = modern.recv().await.unwrap();
    assert_eq!((c, body), (codes::BLOCK_HEADERS, vec![0xc2, 0x4d, 0xc0]));
    let (c, body) = legacy.recv().await.unwrap();
    assert_eq!((c, body), (codes::BLOCK_HEADERS, vec![0xc0]));
    // a message that does not fit the session's framing is not sent
    assert!(node.send(&peer_id(&key(3)), reply(Some(1))).is_ok()); // queued; encode fails inside the session and is skipped
    assert!(matches!(
        node.send(&PeerId::ZERO, reply(None)),
        Err(NetError::UnknownPeer)
    ));
}

#[tokio::test]
async fn bft_messages_are_delivered_once_to_the_app_and_once_to_each_peer() {
    let (node, mut rx) = Peers::new(cfg(), key(1));
    let mut f: Vec<Fake> = Vec::new();
    for i in 0..3 {
        f.push(join(&node, key(10 + i)).await);
    }
    let ids: Vec<PeerId> = (0..3).map(|i| peer_id(&key(10 + i))).collect();

    // our own vote: one copy per peer, a second broadcast sends nothing
    let mine = vote(1);
    assert_eq!(node.broadcast_bft(&mine), 3);
    assert_eq!(node.broadcast_bft(&mine), 0);
    for p in &mut f {
        let (c, body) = p.recv().await.unwrap();
        assert_eq!((c, body), (codes::VOTE, mine.encode_body()));
    }

    // peer 0 sends vote 2; peers 1 and 2 send the same bytes. The app gets it once.
    let v2 = vote(2);
    for p in &mut f {
        p.send(codes::VOTE, &v2.encode_body()).await;
    }
    // a marker after them proves they were all processed
    f[2].send(
        codes::GET_BLOCK_HEADERS,
        &headers_request(Framing::RequestId, 1),
    )
    .await;
    let mut got = Vec::new();
    loop {
        let (peer, m) = next_msg(&mut rx).await;
        if matches!(m, Message::GetBlockHeaders { .. }) {
            break;
        }
        got.push((peer, m));
    }
    assert_eq!(got, vec![(ids[0], Message::Bft(v2.clone()))]);
    // after verification the app relays it: all three already know it, nobody gets it again
    assert_eq!(node.relay_bft(&v2, &ids[0]), 0);
    // a vote that only peer 0 sent goes to the other two
    let v3 = vote(3);
    f[0].send(codes::VOTE, &v3.encode_body()).await;
    let (_, m) = next_msg(&mut rx).await;
    assert_eq!(m, Message::Bft(v3.clone()));
    assert_eq!(node.relay_bft(&v3, &ids[0]), 2);
    for p in &mut f[1..] {
        assert_eq!(p.recv().await.unwrap(), (codes::VOTE, v3.encode_body()));
    }
    assert_eq!(node.relay_bft(&v3, &ids[0]), 0);
}

#[tokio::test]
async fn trusted_peers_get_preference_but_not_leniency() {
    let (node, mut rx) = Peers::new(cfg(), key(1));
    let trusted = peer_id(&key(20));
    node.add_trusted(trusted);

    // 1. a trusted peer sending an undecodable data-plane message is disconnected
    let mut t = join(&node, key(20)).await;
    t.send(codes::GET_BLOCK_HEADERS, &[0x80, 0x80, 0xff]).await;
    assert_eq!(
        t.until_disconnect().await,
        DisconnectReason::SubprotocolError
    );
    expect_disconnected(&mut rx, trusted).await;
    assert_eq!(node.peer_count(), 0);

    // 2. so is one that sends a frame above its size cap (a 2 KiB vote; the cap is 1 KiB)
    let mut t = join(&node, key(20)).await;
    t.send(codes::VOTE, &vec![0u8; 2048]).await;
    assert_eq!(
        t.until_disconnect().await,
        DisconnectReason::SubprotocolError
    );
    expect_disconnected(&mut rx, trusted).await;

    // 3. malformed votes are scored, not fatal one by one: nine are tolerated, about ten end
    //    the session, and the trusted peer is not banned: it connects again at once
    let mut t = join(&node, key(20)).await;
    for _ in 0..9 {
        t.send(codes::VOTE, &[0xc0]).await;
    }
    t.send(
        codes::GET_BLOCK_HEADERS,
        &headers_request(Framing::RequestId, 5),
    )
    .await;
    assert!(matches!(
        next_msg(&mut rx).await.1,
        Message::GetBlockHeaders { id: Some(5), .. }
    ));
    // (the score decays 10 points a minute, so the tenth message is a hair short of 100: send a few more)
    for _ in 0..3 {
        t.send(codes::VOTE, &[0xc0]).await;
    }
    assert_eq!(
        t.until_disconnect().await,
        DisconnectReason::SubprotocolError
    );
    expect_disconnected(&mut rx, trusted).await;
    let t = join(&node, key(20)).await;
    drop(t);

    // 4. an ordinary peer with the same behaviour is banned on top of being disconnected
    let ordinary = peer_id(&key(21));
    let mut o = join(&node, key(21)).await;
    for _ in 0..12 {
        o.send(codes::VOTE, &[0xc0]).await;
    }
    assert_eq!(
        o.until_disconnect().await,
        DisconnectReason::SubprotocolError
    );
    expect_disconnected(&mut rx, ordinary).await;
    let (r, _) = join_with(&node, key(21), both(), 64 * 1024, |v| status(v, NET)).await;
    assert!(
        matches!(r, Err(NetError::Rejected(DisconnectReason::UselessPeer))),
        "{r:?}"
    );
}

#[tokio::test]
async fn trusted_peer_takes_a_slot_from_an_ordinary_peer() {
    let mut c = cfg();
    c.max_peers = 2;
    let (node, mut rx) = Peers::new(c, key(1));
    node.add_static(peer_id(&key(30)));
    let mut a = join(&node, key(10)).await;
    let _b = join(&node, key(11)).await;
    // full: an ordinary third peer is refused
    let (r, _) = join_with(&node, key(12), both(), 64 * 1024, |v| status(v, NET)).await;
    assert!(matches!(
        r,
        Err(NetError::Rejected(DisconnectReason::TooManyPeers))
    ));
    // the static peer evicts the newest ordinary one (key 11)
    let _s = join(&node, key(30)).await;
    assert_eq!(node.peer_count(), 2);
    assert!(
        node.peer_ids().contains(&peer_id(&key(30)))
            && node.peer_ids().contains(&peer_id(&key(10)))
    );
    // the older ordinary peer was not touched
    a.send(
        codes::GET_BLOCK_HEADERS,
        &headers_request(Framing::RequestId, 3),
    )
    .await;
    assert_eq!(next_msg(&mut rx).await.0, peer_id(&key(10)));
}

#[tokio::test]
async fn per_peer_queues_are_bounded() {
    // the app never reads events (capacity 1), so requests pile up in the peer's own queue
    let mut c = cfg();
    c.event_queue = 1;
    let (node, _rx) = Peers::new(c, key(1));
    let mut f = join(&node, key(2)).await;
    // Q7: 8 requests we must serve; the 9th is a flood
    for i in 0..9 {
        f.send(
            codes::GET_BLOCK_HEADERS,
            &headers_request(Framing::RequestId, i),
        )
        .await;
    }
    assert_eq!(
        f.until_disconnect().await,
        DisconnectReason::SubprotocolError
    );
}

/// snap/1 and xpre/1 sit after the 229 eth codes in name order; a peer that shares only snap/1
/// (geth) gets snap at 229 and never an xpre/1 frame; a request reaches the app as `Message::Sub`.
#[tokio::test]
async fn snap_and_xpre_codes_follow_eth() {
    use xone_net::rlpx::Sub;
    let (node, mut rx) = Peers::new(cfg(), key(1));
    // an xOne peer: both capabilities
    let mut caps = both();
    caps.extend([Sub::Snap.cap(), Sub::Xpre.cap()]);
    let (r, f) = join_with(&node, key(2), caps, 64 * 1024, |v| status(v, NET)).await;
    let (xone, mut f) = (r.unwrap(), f.unwrap());
    assert_eq!(f.conn.subs, vec![(Sub::Snap, 229), (Sub::Xpre, 237)]);
    match next_event(&mut rx).await {
        Event::Connected { subs, .. } => assert_eq!(subs, vec![Sub::Snap, Sub::Xpre]),
        e => panic!("{e:?}"),
    }
    f.conn.send(229 + 2, b"\xc1\x01").await.unwrap(); // snap GetStorageRanges
    f.conn.send(237, b"\xc1\x07").await.unwrap(); // xpre GetPreimages
    let (_, m) = next_msg(&mut rx).await;
    assert_eq!(
        m,
        Message::Sub {
            sub: Sub::Snap,
            code: 2,
            body: vec![0xc1, 0x01].into()
        }
    );
    let (_, m) = next_msg(&mut rx).await;
    assert!(matches!(
        m,
        Message::Sub {
            sub: Sub::Xpre,
            code: 0,
            ..
        }
    ));
    node.send(
        &xone,
        Message::Sub {
            sub: Sub::Xpre,
            code: 1,
            body: vec![0xc0].into(),
        },
    )
    .unwrap();
    assert_eq!(f.recv().await.unwrap(), (238, vec![0xc0]));

    // a geth-like peer: snap/1 only
    let mut caps = both();
    caps.push(Sub::Snap.cap());
    let (r, f) = join_with(&node, key(3), caps, 64 * 1024, |v| status(v, NET)).await;
    let (geth, mut f) = (r.unwrap(), f.unwrap());
    assert_eq!(f.conn.subs, vec![(Sub::Snap, 229)]);
    for (sub, code) in [(Sub::Xpre, 1), (Sub::Snap, 3)] {
        node.send(
            &geth,
            Message::Sub {
                sub,
                code,
                body: vec![0xc0].into(),
            },
        )
        .unwrap();
    }
    // the xpre frame was not sent: the first frame is the snap answer
    assert_eq!(f.recv().await.unwrap(), (232, vec![0xc0]));
}

// ---- required blocks (--eth.requiredblocks; geth eth/handler.go) ----

fn hdr(number: u64, salt: u8) -> xone_primitives::Header {
    xone_primitives::Header {
        number,
        extra_data: vec![salt].into(),
        ..Default::default()
    }
}

fn required_cfg(entries: &[(u64, B256)]) -> Config {
    let mut c = cfg();
    c.required_blocks = entries.to_vec();
    c.required_timeout = Duration::from_millis(400);
    c
}

/// The challenge of the node as `(number, id)`.
async fn read_challenge(f: &mut Fake, framing: Framing) -> (u64, Option<u64>) {
    let (code, body) = f.recv().await.unwrap();
    assert_eq!(code, codes::GET_BLOCK_HEADERS);
    match eth::decode(framing, code, &body).unwrap() {
        Message::GetBlockHeaders {
            id,
            query:
                GetHeaders {
                    origin: Origin::Number(n),
                    amount: 1,
                    skip: 0,
                    reverse: false,
                },
        } => (n, id),
        m => panic!("not a one-header challenge: {m:?}"),
    }
}

async fn answer(
    f: &mut Fake,
    framing: Framing,
    id: Option<u64>,
    headers: Vec<xone_primitives::Header>,
) {
    let body = eth::encode(framing, &Message::BlockHeaders { id, headers })
        .unwrap()
        .1;
    f.send(codes::BLOCK_HEADERS, &body).await;
}

#[tokio::test]
async fn a_peer_with_the_required_hashes_stays_and_the_app_never_sees_the_answers() {
    let (a, b) = (hdr(5, 1), hdr(9, 2));
    let (node, mut rx) = Peers::new(required_cfg(&[(5, a.hash()), (9, b.hash())]), key(1));
    let mut f = join(&node, key(2)).await;
    let (n, id) = read_challenge(&mut f, Framing::RequestId).await;
    assert_eq!(n, 5);
    answer(&mut f, Framing::RequestId, id, vec![a]).await;
    let (n, id) = read_challenge(&mut f, Framing::RequestId).await;
    assert_eq!(n, 9);
    answer(&mut f, Framing::RequestId, id, vec![b]).await;
    // a later request of the peer reaches the app, the answers did not
    f.send(
        codes::GET_BLOCK_HEADERS,
        &headers_request(Framing::RequestId, 7),
    )
    .await;
    let (_, m) = next_msg(&mut rx).await;
    assert!(
        matches!(m, Message::GetBlockHeaders { id: Some(7), .. }),
        "{m:?}"
    );
    assert_eq!((node.peer_count(), node.required_refused()), (1, 0));
}

#[tokio::test]
async fn a_peer_with_another_hash_at_a_required_height_is_dropped_with_reason_3() {
    let (node, mut rx) = Peers::new(required_cfg(&[(5, hdr(5, 1).hash())]), key(1));
    let mut f = join(&node, key(2)).await;
    let (_, id) = read_challenge(&mut f, Framing::RequestId).await;
    answer(&mut f, Framing::RequestId, id, vec![hdr(5, 99)]).await;
    assert_eq!(f.until_disconnect().await, DisconnectReason::UselessPeer);
    expect_disconnected(&mut rx, peer_id(&key(2))).await;
    assert_eq!((node.peer_count(), node.required_refused()), (0, 1));
}

#[tokio::test]
async fn a_legacy_eth100_peer_is_challenged_without_a_request_id() {
    let (node, mut rx) = Peers::new(required_cfg(&[(5, hdr(5, 1).hash())]), key(1));
    let (r, f) = join_with(&node, key(3), vec![Cap::eth(100)], 64 * 1024, |v| {
        status(v, NET)
    })
    .await;
    r.unwrap();
    let mut f = f.unwrap();
    let (n, id) = read_challenge(&mut f, Framing::Bare).await;
    assert_eq!((n, id), (5, None));
    answer(&mut f, Framing::Bare, None, vec![hdr(5, 98)]).await;
    assert_eq!(f.until_disconnect().await, DisconnectReason::UselessPeer);
    expect_disconnected(&mut rx, peer_id(&key(3))).await;
    assert_eq!(node.required_refused(), 1);
}

#[tokio::test]
async fn a_peer_that_has_not_reached_the_height_is_kept() {
    let (node, _rx) = Peers::new(required_cfg(&[(5, hdr(5, 1).hash())]), key(1));
    let mut f = join(&node, key(2)).await;
    let (_, id) = read_challenge(&mut f, Framing::RequestId).await;
    answer(&mut f, Framing::RequestId, id, vec![]).await;
    tokio::time::sleep(Duration::from_millis(700)).await; // past the timeout
    assert_eq!((node.peer_count(), node.required_refused()), (1, 0));
}

#[tokio::test]
async fn a_peer_that_does_not_answer_is_dropped_at_the_timeout() {
    let (node, mut rx) = Peers::new(required_cfg(&[(5, hdr(5, 1).hash())]), key(1));
    let mut f = join(&node, key(2)).await;
    let _ = read_challenge(&mut f, Framing::RequestId).await;
    let t = Instant::now();
    assert_eq!(f.until_disconnect().await, DisconnectReason::UselessPeer);
    assert!(t.elapsed() < Duration::from_secs(5));
    expect_disconnected(&mut rx, peer_id(&key(2))).await;
    assert_eq!(node.required_refused(), 1);
}

#[tokio::test]
async fn no_required_blocks_means_no_challenge() {
    let (node, mut rx) = Peers::new(cfg(), key(1));
    let mut f = join(&node, key(2)).await;
    f.send(
        codes::GET_BLOCK_HEADERS,
        &headers_request(Framing::RequestId, 7),
    )
    .await;
    let (_, m) = next_msg(&mut rx).await;
    assert!(matches!(m, Message::GetBlockHeaders { id: Some(7), .. }));
}

/// Node permissioning (issue #199): an unlisted node is refused in and out, and removing a
/// connected peer from the list closes it at once.
#[tokio::test]
async fn allowlist_refuses_unlisted_nodes_in_and_out() {
    let (node, _rx) = Peers::new(cfg(), key(1));
    node.set_allowlist(Some(vec![peer_id(&key(2))]));
    // inbound from an unlisted node
    let (r, _) = join_with(&node, key(3), both(), 64 * 1024, |v| status(v, NET)).await;
    assert!(matches!(
        r,
        Err(NetError::Rejected(DisconnectReason::UselessPeer))
    ));
    // outbound to an unlisted node: refused before any socket
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 9));
    assert!(matches!(
        node.dial(addr, peer_id(&key(3))).await,
        Err(NetError::Rejected(DisconnectReason::UselessPeer))
    ));
    // a listed node connects
    let mut f = join(&node, key(2)).await;
    assert_eq!(node.peer_count(), 1);
    // removed at runtime: closed within one second
    let t0 = Instant::now();
    node.set_allowlist(Some(Vec::new()));
    assert_eq!(f.until_disconnect().await, DisconnectReason::UselessPeer);
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(node.peer_count(), 0);
    // off again: anyone may connect
    node.set_allowlist(None);
    assert!(node.is_allowed(&peer_id(&key(3))));
    let _g = join(&node, key(3)).await;
}
