#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
#![allow(clippy::disallowed_methods)] // the ignored live test finds its node through the environment
//! Interop with a local geth-XDC node (loopback only, no discovery, no public network).
//! Ignored by default. Start the node with `tests/local-geth/run.sh` (see its header for the
//! one-time chain preparation), then
//!
//! ```text
//! XONE_GETH_ENODE=$(cat $DATADIR/enode.txt) XONE_GETH_GENESIS=$(cat $DATADIR/genesis.txt) \
//!   XONE_GETH_LOG=$DATADIR/geth.log \
//!   cargo test --locked -p xone-net --test local_geth -- --ignored --nocapture --test-threads=1
//! ```

use std::time::Duration;

use alloy_primitives::{B256, U256};
use reth_network_peers::NodeRecord;
use secp256k1::SecretKey;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;
use xone_net::eth::{BftMessage, GetHeaders, Message, Origin};
use xone_net::peers::{Config, Event, Peers};
use xone_net::rlpx::PeerId;
use xone_primitives::{BlockInfo, Header, Vote};

const NETWORK_ID: u64 = 5551;
const T: Duration = Duration::from_secs(20);
/// Blocks 0 to 10.
const COUNT: u64 = 11;
/// keccak256(rlp("")): the root of an empty transaction or receipt trie.
const EMPTY_ROOT: B256 =
    alloy_primitives::b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}, see the module docs"))
}

fn node() -> NodeRecord {
    env("XONE_GETH_ENODE").parse().expect("enode url")
}

fn genesis() -> B256 {
    env("XONE_GETH_GENESIS").parse().expect("genesis hash")
}

fn key() -> SecretKey {
    // a throwaway identity for this run only
    SecretKey::from_slice(B256::random().as_slice()).unwrap()
}

async fn connect(versions: Vec<u64>) -> (Peers, Receiver<Event>, PeerId, u64) {
    let mut cfg = Config::new("xone-net-test", NETWORK_ID, genesis());
    cfg.td = U256::from(1);
    cfg.versions = versions;
    let (peers, mut rx) = Peers::new(cfg, key());
    let n = node();
    let id = peers
        .dial((n.address, n.tcp_port).into(), n.id)
        .await
        .expect("dial and handshake");
    let version = loop {
        if let Event::Connected {
            peer,
            version,
            rank,
            inbound,
        } = next(&mut rx).await
        {
            assert_eq!((peer, inbound), (n.id, false));
            println!("connected to geth: eth/{version}, rank (TD) {rank}");
            break version;
        }
    };
    assert_eq!(id, n.id);
    (peers, rx, id, version)
}

async fn next(rx: &mut Receiver<Event>) -> Event {
    timeout(T, rx.recv())
        .await
        .expect("event timed out")
        .expect("channel closed")
}

/// The next message of a kind, skipping gossip that a live node sends (NewBlockHashes and so on).
async fn expect(rx: &mut Receiver<Event>, pick: impl Fn(&Message) -> bool) -> Message {
    loop {
        if let Event::Message { msg, .. } = next(rx).await {
            if pick(&msg) {
                return msg;
            }
        }
    }
}

/// Headers, bodies and receipts of blocks 0..=10, checked against each other.
async fn fetch_chain(peers: &Peers, rx: &mut Receiver<Event>, id: PeerId, version: u64) {
    let rid = |n: u64| (version == 101).then_some(n);
    peers
        .send(
            &id,
            Message::GetBlockHeaders {
                id: rid(1),
                query: GetHeaders {
                    origin: Origin::Number(0),
                    amount: COUNT,
                    skip: 0,
                    reverse: false,
                },
            },
        )
        .unwrap();
    let Message::BlockHeaders { id: got, headers } =
        expect(rx, |m| matches!(m, Message::BlockHeaders { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(got, rid(1));
    assert_eq!(
        headers.len(),
        usize::try_from(COUNT).unwrap(),
        "geth has blocks 0..=10"
    );
    for (n, h) in headers.iter().enumerate() {
        assert_eq!(h.number, u64::try_from(n).unwrap());
        if n > 0 {
            assert_eq!(
                h.parent_hash,
                headers[n - 1].hash(),
                "block {n} links to block {}",
                n - 1
            );
        }
    }
    assert_eq!(
        headers[0].hash(),
        genesis(),
        "block 0 hashes to the genesis hash"
    );
    println!("headers 0..=10: ok, head {}", headers[10].hash());
    let hashes: Vec<B256> = headers.iter().map(Header::hash).collect();

    peers
        .send(
            &id,
            Message::GetBlockBodies {
                id: rid(2),
                hashes: hashes.clone(),
            },
        )
        .unwrap();
    let Message::BlockBodies { id: got, bodies } =
        expect(rx, |m| matches!(m, Message::BlockBodies { .. })).await
    else {
        unreachable!()
    };
    assert_eq!((got, bodies.len()), (rid(2), hashes.len()));
    // an empty block has no transactions and no uncles, and its roots are the empty roots
    for (b, h) in bodies.iter().zip(&headers) {
        assert_eq!(b.transactions.is_empty(), h.tx_root == EMPTY_ROOT);
        assert!(b.uncles.is_empty());
    }
    println!(
        "bodies 0..=10: ok ({} transactions)",
        bodies.iter().map(|b| b.transactions.len()).sum::<usize>()
    );

    peers
        .send(
            &id,
            Message::GetReceipts {
                id: rid(3),
                first_index: 0,
                hashes,
            },
        )
        .unwrap();
    let Message::Receipts {
        id: got,
        per_block,
        last_incomplete,
    } = expect(rx, |m| matches!(m, Message::Receipts { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(
        (got, per_block.len(), last_incomplete),
        (rid(3), headers.len(), false)
    );
    println!("receipts 0..=10: ok ({} lists)", per_block.len());
}

async fn wait_log(needle: &str) -> Option<String> {
    let path = std::env::var("XONE_GETH_LOG").ok()?;
    for _ in 0..20 {
        let log = std::fs::read_to_string(&path).unwrap();
        if let Some(l) = log.lines().rev().find(|l| l.contains(needle)) {
            return Some(l.to_owned());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

#[tokio::test]
#[ignore = "needs a local geth-XDC node, see tests/local-geth/run.sh"]
async fn eth_101_headers_bodies_receipts_and_vote() {
    let (peers, mut rx, id, version) = connect(vec![101, 100]).await;
    assert_eq!(version, 101, "101 is offered first and negotiated");
    fetch_chain(&peers, &mut rx, id, version).await;

    // A Vote: geth decodes it into types.Vote and hands it to the engine (verification fails on
    // a node without the voter's snapshot; the peer stays connected).
    let vote = Vote {
        proposed_block_info: BlockInfo {
            hash: B256::repeat_byte(0x11),
            round: 1,
            number: 1,
        },
        signature: vec![0u8; 65].into(),
        gap_number: 0,
    };
    assert_eq!(peers.broadcast_bft(&BftMessage::Vote(vote.clone())), 1);
    if let Some(line) = wait_log("Receive Vote").await {
        println!("geth: {line}");
        assert!(
            line.contains(&format!("{:#x}", vote.proposed_block_info.hash)),
            "{line}"
        );
        assert!(
            line.contains("number=1") && line.contains("round=1"),
            "{line}"
        );
    } else if std::env::var("XONE_GETH_LOG").is_ok() {
        panic!("geth did not log the Vote");
    }
    // the same vote again is not sent twice
    assert_eq!(peers.broadcast_bft(&BftMessage::Vote(vote)), 0);
    // still connected: one more header round trip
    fetch_chain(&peers, &mut rx, id, version).await;
    let report = peers.shutdown().await;
    assert_eq!((report.closed, report.aborted), (1, 0));
}

#[tokio::test]
#[ignore = "needs a local geth-XDC node, see tests/local-geth/run.sh"]
async fn eth_100_only_session() {
    // offer only eth/100: geth falls back to the bare, request-id-less shape
    let (peers, mut rx, id, version) = connect(vec![100]).await;
    assert_eq!(version, 100);
    fetch_chain(&peers, &mut rx, id, version).await;
    let report = peers.shutdown().await;
    assert_eq!((report.closed, report.aborted), (1, 0));
}

/// Chain preparation (see `tests/local-geth/run.sh`, MODE=prepare): a peer whose head is the
/// genesis, kept connected, so that the local miner leaves its sync gate. Answers every request
/// with an empty response.
#[tokio::test]
#[ignore = "chain preparation, run by tests/local-geth/run.sh"]
async fn hold_genesis_peer() {
    let mut cfg = Config::new("xone-net-holder", NETWORK_ID, genesis());
    cfg.td = U256::from(1);
    let (peers, mut rx) = Peers::new(cfg, key());
    let n = node();
    peers
        .dial((n.address, n.tcp_port).into(), n.id)
        .await
        .expect("dial");
    let hold = Duration::from_secs(900);
    let _ = timeout(hold, async {
        while let Some(ev) = rx.recv().await {
            if let Event::Message { peer, msg } = ev {
                let reply = match msg {
                    Message::GetBlockHeaders { id, .. } => Message::BlockHeaders {
                        id,
                        headers: vec![],
                    },
                    Message::GetBlockBodies { id, .. } => {
                        Message::BlockBodies { id, bodies: vec![] }
                    }
                    Message::GetReceipts { id, .. } => Message::Receipts {
                        id,
                        last_incomplete: false,
                        per_block: vec![],
                    },
                    _ => continue,
                };
                let _ = peers.send(&peer, reply);
            }
        }
    })
    .await;
}
