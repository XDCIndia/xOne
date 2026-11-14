#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! `--netrestrict` (#175) through the real entry points: `Peers::dial`, `Peers::accept` and
//! `Discv4`.

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use secp256k1::SecretKey;
use tokio::{net::TcpListener, time::timeout};
use xone_net::{
    discv4::Discv4,
    peers::{Config, Peers},
    rlpx::peer_id,
};

fn key(i: u8) -> SecretKey {
    SecretKey::from_slice(&[i; 32]).unwrap()
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
/// #175: `--netrestrict` refuses dials and inbound connections outside the list and keeps
/// discovery inside it; a peer inside the list connects.
#[tokio::test]
async fn netrestrict_keeps_dials_inbound_and_discovery_inside_the_list() {
    use xone_net::{netrestrict::NetRestrict, NetError};
    let restricted = |list: &str| {
        let mut c = Config::new("xone-test", 5551, B256::repeat_byte(0x42));
        c.restrict = Some(list.parse::<NetRestrict>().unwrap());
        c
    };
    let open = || Config::new("xone-test", 5551, B256::repeat_byte(0x42));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // a node restricted to 10.0.0.0/8 neither dials 127.0.0.1 ...
    let (a, _ra) = Peers::new(restricted("10.0.0.0/8"), key(5));
    let e = a.dial(addr, peer_id(&key(6))).await.unwrap_err();
    assert!(matches!(e, NetError::NetRestricted(_)), "{e}");
    // ... nor accepts from it, before any handshake
    let acceptor = a.clone();
    let accepted = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        acceptor.accept(tcp).await
    });
    let (b, _rb) = Peers::new(open(), key(6));
    assert!(b.dial(addr, a.id()).await.is_err());
    assert!(matches!(
        accepted.await.unwrap(),
        Err(NetError::NetRestricted(_))
    ));
    assert!(a.peer_ids().is_empty());

    // a node restricted to 127.0.0.0/8 accepts a loopback peer
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (c, _rc) = Peers::new(restricted("127.0.0.0/8"), key(7));
    let acceptor = c.clone();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = acceptor.accept(tcp).await;
    });
    let (d, _rd) = Peers::new(restricted("127.0.0.0/8"), key(8));
    assert_eq!(
        timeout(Duration::from_secs(10), d.dial(addr, c.id()))
            .await
            .expect("dial timed out")
            .unwrap(),
        c.id()
    );

    // discovery: a restricted node drops a ping from outside and pings nobody outside
    let outside: SocketAddr = "192.0.2.1:30303".parse().unwrap();
    let inside: SocketAddr = "10.0.0.2:30303".parse().unwrap();
    let mut far = Discv4::new(key(9), outside, 30303);
    let mut near = Discv4::new(key(10), "10.0.0.1:30303".parse().unwrap(), 30303)
        .with_restrict(Some("10.0.0.0/8".parse().unwrap()));
    let mut wire: Vec<(SocketAddr, Vec<u8>)> = vec![];
    far.ping(now_ms(), inside, near.id(), &mut wire);
    let (_, ping) = wire.pop().unwrap();
    let mut replies: Vec<(SocketAddr, Vec<u8>)> = vec![];
    assert!(near.handle(now_ms(), outside, &ping, &mut replies).is_err());
    assert!(replies.is_empty());
    near.ping(now_ms(), outside, far.id(), &mut replies);
    assert!(replies.is_empty(), "pinged a node outside the list");
    near.ping(now_ms(), inside, peer_id(&key(11)), &mut replies);
    assert_eq!(replies.len(), 1);
}
