#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
#![allow(clippy::disallowed_methods)] // the ignored live test reads its peer from the environment
//! The public net2040 follow (docs/ops/net2040-follow.md) in small: `--network net2040` from genesis
//! against the geth-XDC archive peer, through the first V1 checkpoint (900) and the first dense blocks
//! (916..967, up to 9,761 transfers each), to block 1,000, whose hash is the archive's. Ignored: it
//! needs the public peer and a few minutes.
//!
//! ```text
//! cargo test --release --locked -p xone-node --test live_net2040 -- --ignored --nocapture
//! ```
//! `XONE_NET2040_ENODE` overrides the peer.

mod common;

use std::time::{Duration, Instant};

use alloy_primitives::b256;
use common::{call, free_port, scratch};
use serde_json::json;
use xone_node::{datadir, services, Config};

const ARCHIVE: &str = "enode://0337f630b6f23f2551d068f13b2217fb766ee242087e70b699b6048e368c2c32bd7e60af95f5fc2b027e90e4096c130c7f9d3282dcfa67583c31c4bb6ba504a9@135.181.117.109:35071";

#[test]
#[ignore = "needs the public net2040 archive peer; see the module docs"]
fn follows_net2040_from_genesis_to_block_1000() {
    let dir = scratch("net2040");
    let port = free_port();
    let mut cfg = Config::default();
    cfg.set("network", "net2040").unwrap();
    let enode = std::env::var("XONE_NET2040_ENODE").unwrap_or_else(|_| ARCHIVE.to_owned());
    cfg.set("bootnodes", &enode).unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.http.enabled = true;
    cfg.http.port = port;
    cfg.http.api = vec!["eth".into(), "XDPoS".into()];
    cfg.ipc.disabled = true;
    let (marker, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    // block 0 of the archive (`eth_getBlockByNumber(0)`)
    assert_eq!(
        marker.genesis_hash,
        "0x8d8ff2e9d8369b7b4af5a84eb010e2b1a358eefa9f971985435d538dda235714"
    );
    let mut built = services::build(&cfg, &marker, spec, &genesis, "xone-test").unwrap();
    built.node.start().unwrap();

    let t = Instant::now();
    while built.core.head().0 < 1000 {
        assert!(
            t.elapsed() < Duration::from_secs(1200),
            "head {:?}",
            built.core.head()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    // block 1,000 of the archive (`eth_getBlockByNumber(0x3e8)`)
    let want = b256!("df4144e177a5c95d940d202f12fcb44a620e898fe37102a474f488e41248ab91");
    let b = call(port, "eth_getBlockByNumber", json!(["0x3e8", false]));
    assert_eq!(b["hash"], json!(want));
    assert_eq!(
        b["stateRoot"],
        json!(b256!(
            "93c710f232a35c2f9d36f4aa31f3689442cadc3a11d1ebd2ce9aa92b51a275d1"
        ))
    );
    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
