//! Handshake, status, header round trip and xdpos2 messages against a local geth-XDC node.
//! Ignored by default: start the node with `run-local-geth.sh`, then
//!   XONE_GETH_ENODE=$(cat $DATADIR/enode.txt) XONE_GETH_LOG=$DATADIR/geth.log \
//!     cargo test -p net-spike --test local_geth -- --ignored --nocapture --test-threads=1

use alloy_primitives::{B256, U256};
use alloy_rlp::{Decodable, Encodable};
use net_spike::*;
use reth_network_peers::NodeRecord;
use secp256k1::SecretKey;
use xone_chainspec::Network;
use xone_primitives::{BlockInfo, Header, Vote};

/// XDC devnet, network id 5551: what `geth --xdcdevnet` runs (`core/genesis_xdc.go:84`).
const NETWORK_ID: u64 = 5551;

fn node() -> NodeRecord {
    std::env::var("XONE_GETH_ENODE")
        .expect("XONE_GETH_ENODE")
        .parse()
        .expect("enode url")
}

fn genesis_hash() -> B256 {
    let spec = Network::Devnet.spec();
    Network::Devnet.genesis().unwrap().hash(&spec).unwrap()
}

async fn dial(caps: Vec<Cap>) -> Session {
    // a throwaway identity for this connection only
    let key = SecretKey::from_slice(B256::random().as_slice()).unwrap();
    let n = node();
    Session::connect((n.address, n.tcp_port).into(), n.id, key, caps)
        .await
        .expect("session")
}

async fn status(s: &mut Session) -> Status62 {
    let genesis = genesis_hash();
    let ours = Status62 {
        version: s.eth_version as u32,
        network_id: NETWORK_ID,
        td: U256::from(1),
        head: genesis,
        genesis,
    };
    s.send_eth(STATUS, &alloy_rlp::encode(&ours)).await.unwrap();
    let body = s.expect_eth(STATUS).await.unwrap();
    // strict five-field decode: a fork id would make this fail
    let theirs = Status62::decode(&mut &body[..]).expect("five-field status");
    assert_eq!(theirs.version as u64, s.eth_version);
    assert_eq!(theirs.network_id, NETWORK_ID);
    assert_eq!(
        theirs.genesis,
        genesis_hash(),
        "geth genesis equals xone-chainspec genesis"
    );
    theirs
}

/// Ask for block 0 and return the decoded header hash.
async fn genesis_header(s: &mut Session) -> B256 {
    let req = GetHeaders {
        origin: 0,
        amount: 1,
        skip: 0,
        reverse: false,
    };
    let body = if s.eth_version == ETH_101 {
        alloy_rlp::encode(&GetHeadersRid {
            request_id: 42,
            request: req,
        })
    } else {
        alloy_rlp::encode(&req)
    };
    s.send_eth(GET_BLOCK_HEADERS, &body).await.unwrap();
    let mut b = &s.expect_eth(BLOCK_HEADERS).await.unwrap()[..];
    let headers: Vec<Header> = if s.eth_version == ETH_101 {
        // [request id, [headers]]
        let list = alloy_rlp::Header::decode(&mut b).unwrap();
        assert!(list.list);
        assert_eq!(u64::decode(&mut b).unwrap(), 42);
        Vec::decode(&mut b).unwrap()
    } else {
        // eth/100: the bare list
        Vec::decode(&mut b).unwrap()
    };
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].number, 0);
    headers[0].hash()
}

#[tokio::test]
#[ignore = "needs a local geth-XDC node, see run-local-geth.sh"]
async fn eth_101_status_headers_and_consensus_messages() {
    let mut s = dial(vec![Cap::eth(ETH_101), Cap::eth(ETH_100)]).await;
    println!("peer hello: {} caps {:?}", s.peer.name, s.peer.caps);
    assert_eq!(s.eth_version, ETH_101);
    // a separate `xdpos2` capability name does not exist at the pin: it is eth/100 and eth/101
    assert!(s.peer.caps.iter().all(|c| c.name != "xdpos2"));
    status(&mut s).await;
    assert_eq!(genesis_header(&mut s).await, genesis_hash());

    // xdpos2 consensus messages: sent as the bare RLP body at code 0xe0..0xe2 (handlers.go:1158-1190).
    // geth decodes it into types.Vote and verifies it; verification fails (no snapshot on a fresh
    // node) but the peer is kept, which is the same outcome as for any unverifiable vote.
    let vote = Vote {
        proposed_block_info: BlockInfo {
            hash: B256::repeat_byte(0x11),
            round: 1,
            number: 1,
        },
        signature: vec![0u8; 65].into(),
        gap_number: 0,
    };
    let mut v = Vec::new();
    vote.encode(&mut v);
    s.send_eth(VOTE, &v).await.unwrap();
    // the session is alive after the Vote: one more header round trip
    assert_eq!(genesis_header(&mut s).await, genesis_hash());
    s.disconnect().await.unwrap();
    println!("vote bytes sent: {}", v.len());
    if let Ok(path) = std::env::var("XONE_GETH_LOG") {
        let log = std::fs::read_to_string(path).unwrap();
        // geth decoded our bytes into types.Vote and handed it to the engine (trace log, verbosity 5)
        let line = log
            .lines()
            .rev()
            .find(|l| l.contains("Receive Vote"))
            .expect("geth logged the Vote");
        println!("geth: {line}");
        assert!(
            line.contains(&format!("{:#x}", vote.proposed_block_info.hash)),
            "{line}"
        );
        assert!(
            line.contains("number=1") && line.contains("round=1"),
            "{line}"
        );
    }
}

#[tokio::test]
#[ignore = "needs a local geth-XDC node, see run-local-geth.sh"]
async fn eth_100_bare_framing() {
    // offer only eth/100: the node must fall back to the bare, request-id-less shape
    let mut s = dial(vec![Cap::eth(ETH_100)]).await;
    assert_eq!(s.eth_version, ETH_100);
    status(&mut s).await;
    assert_eq!(genesis_header(&mut s).await, genesis_hash());
    s.disconnect().await.unwrap();
}
