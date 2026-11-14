#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Two nodes over IPv6 loopback: an RLPx session over `[::1]` TCP, and discv4 bonding between
//! `[::1]` UDP sockets. Skipped (passes) where the host has no IPv6 loopback.

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use secp256k1::SecretKey;
use tokio::{
    net::{TcpListener, UdpSocket},
    time::{sleep, timeout},
};
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

#[tokio::test]
async fn two_nodes_connect_and_bond_over_ipv6_loopback() {
    let Ok(listener) = TcpListener::bind("[::1]:0").await else {
        eprintln!("no IPv6 loopback on this host: skipped");
        return;
    };
    let cfg = || Config::new("xone-test", 5551, B256::repeat_byte(0x42));
    let (a, _rx_a) = Peers::new(cfg(), key(1));
    let (b, _rx_b) = Peers::new(cfg(), key(2));
    let addr = listener.local_addr().unwrap();
    assert!(addr.is_ipv6());
    let acceptor = a.clone();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = acceptor.attach(tcp, None).await;
    });
    let got = timeout(Duration::from_secs(10), b.dial(addr, a.id()))
        .await
        .expect("dial timed out")
        .unwrap();
    assert_eq!(got, a.id());
    timeout(Duration::from_secs(10), async {
        while !a.peer_ids().contains(&b.id()) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the listener never saw the dialer");

    // discv4: a pings b; both answer until each holds the other in its table
    let mut sa = UdpSocket::bind("[::1]:0").await.unwrap();
    let mut sb = UdpSocket::bind("[::1]:0").await.unwrap();
    let (aa, ab): (SocketAddr, SocketAddr) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
    let mut da = Discv4::new(key(3), aa, 30303);
    let mut db = Discv4::new(key(4), ab, 30303);
    let (ida, idb) = (peer_id(&key(3)), peer_id(&key(4)));
    let _ = sa.writable().await;
    let _ = sb.writable().await;
    da.ping(now_ms(), ab, idb, &mut sa);
    let (mut ba, mut bb) = (vec![0u8; 2048], vec![0u8; 2048]);
    let bonded = timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                r = sa.recv_from(&mut ba) => {
                    let (n, from) = r.unwrap();
                    let _ = da.handle(now_ms(), from, &ba[..n], &mut sa);
                }
                r = sb.recv_from(&mut bb) => {
                    let (n, from) = r.unwrap();
                    let _ = db.handle(now_ms(), from, &bb[..n], &mut sb);
                }
            }
            if da.table().get(&idb).is_some() && db.table().get(&ida).is_some() {
                break;
            }
        }
    })
    .await;
    assert!(bonded.is_ok(), "discv4 did not bond over IPv6");
    assert_eq!(da.table().get(&idb).unwrap().addr, ab);
}
