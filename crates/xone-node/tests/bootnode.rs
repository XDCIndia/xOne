#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! devp2p tooling (issue #178): two discv4 nodes that know only an xOne bootnode find each other
//! through it, and the key tools round-trip.

use std::{
    net::{SocketAddr, UdpSocket},
    sync::atomic::Ordering::SeqCst,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use secp256k1::SecretKey;
use xone_net::{discv4::Discv4, rlpx::peer_id};
use xone_node::p2p;

struct Client {
    d: Discv4,
    sock: UdpSocket,
    out: Vec<(SocketAddr, Vec<u8>)>,
}

fn now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

impl Client {
    fn new(k: u8) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let local = sock.local_addr().unwrap();
        Self {
            d: Discv4::new(
                SecretKey::from_slice(&[k; 32]).unwrap(),
                local,
                local.port(),
            ),
            sock,
            out: Vec::new(),
        }
    }

    fn pump(&mut self) {
        for (to, data) in self.out.drain(..) {
            let _ = self.sock.send_to(&data, to);
        }
        let mut buf = [0u8; 2048];
        if let Ok((n, from)) = self.sock.recv_from(&mut buf) {
            let _ = self.d.handle(now(), from, &buf[..n], &mut self.out);
        }
        for (to, data) in self.out.drain(..) {
            let _ = self.sock.send_to(&data, to);
        }
    }

    fn knows(&self, id: &xone_net::rlpx::PeerId) -> bool {
        self.d
            .table()
            .closest(&B256::ZERO, 64)
            .iter()
            .any(|n| n.id == *id)
    }
}

#[test]
fn nodes_find_each_other_through_the_bootnode() {
    let bkey = p2p::generate_key().unwrap();
    let bid = peer_id(&bkey);
    let (baddr, stop, th) = p2p::spawn_bootnode(bkey, "127.0.0.1:0".parse().unwrap()).unwrap();
    let (mut a, mut b) = (Client::new(7), Client::new(8));
    let (aid, bid_c) = (a.d.id(), b.d.id());
    a.d.ping(now(), baddr, bid, &mut a.out);
    b.d.ping(now(), baddr, bid, &mut b.out);
    let t0 = Instant::now();
    let mut last_find = Instant::now() - Duration::from_secs(1);
    while !(b.knows(&aid) && a.knows(&bid_c)) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "no discovery through the bootnode"
        );
        a.pump();
        b.pump();
        if last_find.elapsed() > Duration::from_millis(300) {
            last_find = Instant::now();
            for c in [&mut a, &mut b] {
                for n in c.d.table().closest(&B256::ZERO, 16) {
                    if n.id == bid {
                        let target = c.d.id();
                        c.d.find_node(now(), &n, target, &mut c.out);
                    }
                }
            }
        }
    }
    stop.store(true, SeqCst);
    th.join().unwrap().unwrap();
}

#[test]
fn key_file_and_enode_url() {
    let dir = std::env::temp_dir().join(format!("xone-p2pkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("nodekey");
    let k = p2p::generate_key().unwrap();
    p2p::write_key(&f, &k).unwrap();
    assert!(p2p::write_key(&f, &k).is_err(), "no overwrite");
    assert_eq!(p2p::read_key(&f).unwrap(), k);
    let id = alloy_primitives::hex::encode(peer_id(&k));
    assert_eq!(
        p2p::enode_url(&k, [10, 0, 0, 1].into(), 30303, 30303),
        format!("enode://{id}@10.0.0.1:30303")
    );
    assert_eq!(
        p2p::enode_url(&k, [10, 0, 0, 1].into(), 30303, 30301),
        format!("enode://{id}@10.0.0.1:30303?discport=30301")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
