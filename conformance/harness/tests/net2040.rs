#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! Sequential replay of net2040 (chain 2040, every fork live within 21,600 blocks) on xOne's own
//! state: the committed blocks 0..=899 (genesis to the block before the first V1 checkpoint, as the
//! archive returned them), and the whole fork cluster from a local block cache.

use std::path::Path;
use xone_chainspec::Network;
use xone_harness::{
    cache::{self, decode},
    replay::{run, Opts},
};

fn opts(to: u64) -> Opts {
    Opts {
        to,
        state_dir: None,
        every: 0,
        resume: false,
        keep: false,
        progress: 0,
        gap_offset: 0,
        dump_state: None,
    }
}

#[test]
fn genesis_to_899_replay_with_no_mismatch() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/replay/net2040-0-899.jsonl.gz");
    let blocks: Vec<_> = cache::read(&p)
        .unwrap()
        .iter()
        .map(|l| decode(l).unwrap())
        .collect();
    assert_eq!(blocks.len(), 900);
    let net = Network::Net2040;
    let r = run(net, &net.spec(), &blocks, &opts(899)).unwrap();
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!((r.last_ok, r.blocks), (899, 899));
}

/// Genesis to 22,000: the denylist, TRC21 and XDCx forks (7,200), Berlin and London (9,000), gas50x
/// and the V2 switch (10,800), eip1559 (12,600), Cancun (14,400), tiered reward and penalties
/// (16,200), the Prague deploy (18,000), Osaka (19,800) and Amsterdam (21,600), with 13 V2 epoch
/// switches. Needs `cache/net2040.jsonl` (`xone-replay fetch --network net2040 --to 22000`, about
/// 400 MB of it above block 7,000 and 3 GB in all, which cannot be committed), about 30 minutes.
#[test]
#[ignore = "needs the net2040 block cache"]
fn fork_cluster_replays_with_no_mismatch() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("cache/net2040.jsonl");
    let blocks: Vec<_> = cache::read_limit(&p, 22_001)
        .unwrap()
        .iter()
        .map(|l| decode(l).unwrap())
        .collect();
    let net = Network::Net2040;
    let r = run(net, &net.spec(), &blocks, &opts(22_000)).unwrap();
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!(r.last_ok, 22_000);
    assert_eq!(r.v2_switches, 13);
}
