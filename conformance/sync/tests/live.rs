#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)] // test tool: env is the interface
//! Follow mode against a loopback geth-XDC that mines a private chain. Ignored by default. Start it
//! with `run.sh` (see `conformance/sync/run-live.sh`): the node's miner leaves its sync gate because
//! this test is a peer whose head is the genesis, then mines one block per 2 s; the test follows
//! it through real RLPx sessions, real eth framing and the real executor, for 50 blocks, and
//! requires the head lag to stay at 2 blocks or less.

use alloy_primitives::{B256, U256};
use secp256k1::SecretKey;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use xone_chainspec::{devnet, ForkBlock, Network};
use xone_consensus::verify::HeaderError;
use xone_net::peers::Config;
use xone_primitives::Header;
use xone_sync::{
    chain::{Chain, ChainRead},
    follow::{Follower, PeerSource},
    testkit::MemStore,
    xdc::{genesis_state, XdcExecutor},
};
use xone_sync_live::NetSource;

const NETWORK_ID: u64 = 5551;
/// Blocks to follow (the head to reach); `XONE_FOLLOW` raises it.
fn follow_target() -> u64 {
    std::env::var("XONE_FOLLOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

type Accept = fn(&dyn ChainRead, &Header, &Header, u64) -> Result<(), HeaderError>;

fn accept(_: &dyn ChainRead, _: &Header, _: &Header, _: u64) -> Result<(), HeaderError> {
    Ok(())
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}, see conformance/sync/run-live.sh"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// `enode://<id>@127.0.0.1:<port>` as written by `run.sh`: node id and socket address.
fn enode(url: &str) -> (alloy_primitives::B512, std::net::SocketAddr) {
    let rest = url.trim().strip_prefix("enode://").expect("enode url");
    let (id, addr) = rest.split_once('@').expect("enode url");
    (
        id.parse().expect("node id"),
        addr.split('?')
            .next()
            .unwrap()
            .parse()
            .expect("socket address"),
    )
}

#[test]
#[ignore = "needs a mining local geth-XDC node, see conformance/sync/run-live.sh"]
fn follows_a_mined_chain_for_50_blocks_with_head_lag_at_most_2() {
    let genesis_hash: B256 = env("XONE_GETH_GENESIS").parse().unwrap();
    let url = env("XONE_GETH_ENODE");
    let (id, addr) = enode(&url);
    let mut cfg = Config::new("xone-follow", NETWORK_ID, genesis_hash);
    cfg.td = U256::from(1);
    let key = SecretKey::from_slice(B256::random().as_slice()).unwrap();
    let mut src = NetSource::new(cfg, key).unwrap();
    let peer = src.dial(addr, id).expect("dial and handshake");

    // the chain starts at the node's genesis block, fetched over the wire and hash-checked
    let g = src.headers_by_number(&peer, 0, 1).unwrap().remove(0);
    assert_eq!(g.hash(), genesis_hash, "block 0 hashes to the genesis hash");
    let (flat, codes) = genesis_state(Network::Devnet).unwrap();
    assert_eq!(
        xone_state::trie::flat_root(&flat),
        g.state_root,
        "the devnet alloc is the node's state"
    );
    // The executor has no TRC21 fee path yet (XB-08) and refuses a chain where `TIPTRC21Fee` is active.
    // The blocks mined here hold no transactions, so the rule cannot act on them, and the state root
    // and receipts checks of `Chain::insert` would show a difference if it did.
    let mut spec = devnet();
    spec.effective.tip_trc21_fee = ForkBlock::Never;
    let exec = XdcExecutor::from_genesis(spec.clone(), g.clone(), codes).unwrap();
    let chain: Chain<MemStore, XdcExecutor, Accept> = Chain::new(
        spec.clone(),
        MemStore::with_anchor(g, flat, 128, 100_000),
        exec,
        accept as Accept,
    );
    let mut follower = Follower::new(&spec);

    let (start, deadline) = (Instant::now(), Instant::now() + Duration::from_secs(900));
    let (mut max_lag, mut samples, mut rejected) = (0u64, vec![], 0u64);
    let mut reported = 0;
    let mut last_print = Instant::now();
    let follow = follow_target();
    let mut live_from = None;
    while chain.head().0 < follow && Instant::now() < deadline {
        let r = follower.step(&chain, &mut src, now()).unwrap();
        rejected += r.rejected;
        assert!(
            r.stalled_at.is_none() || chain.head().0 == 0 || src.announced.0 <= chain.head().0 + 1,
            "stalled: {r:?}"
        );
        let (head, tip) = (chain.head().0, src.announced.0);
        // lag: how far the highest block geth announced is above our head after a step
        let lag = tip.saturating_sub(head);
        if src.announced.0 > 0 && live_from.is_none() {
            live_from = Some(head); // the first push: from here the node is mining under us
        }
        if live_from.is_some() {
            max_lag = max_lag.max(lag);
            samples.push(lag);
        }
        if last_print.elapsed() > Duration::from_secs(10) {
            last_print = Instant::now();
            println!("{:>5.1}s head {head} announced {tip} imported {} rejected {} stalled {:?} {:?} dropped {}", start.elapsed().as_secs_f64(), r.imported, r.rejected, r.stalled_at, r.last_rejection, follower.dropped);
        }
        if head >= reported + 10 {
            reported = head;
            println!(
                "{:>5.1}s head {head} announced {tip} lag {lag} status {:?}",
                start.elapsed().as_secs_f64(),
                r.status
            );
        }
    }
    let head = chain.head();
    assert!(
        head.0 >= follow,
        "followed only {} blocks in {:?}",
        head.0,
        start.elapsed()
    );
    assert_eq!(rejected, 0, "no block of geth was refused");
    samples.sort_unstable();
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    println!(
        "followed {} blocks in {:.1}s ({} by catch-up, {} pushed live), head {} {}, geth's last announced block {} {}, lag samples {}: max {max_lag}, p99 {p99}",
        head.0,
        start.elapsed().as_secs_f64(),
        live_from.unwrap_or(0),
        head.0 - live_from.unwrap_or(0),
        head.0,
        head.1,
        src.announced.0,
        src.announced.1,
        samples.len()
    );
    assert!(max_lag <= 2, "head lag {max_lag} exceeded 2 blocks");
    src.shutdown();
}
