#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! `--debug.logslowblock` (#226): threshold 0 logs every imported block, a threshold no block
//! reaches and the default (off) log none; the import result is the same either way.

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{parts, scratch, vectors};
use xone_chainspec::Network;
use xone_node::services::core::{now, Core};

const N: usize = 6;

fn import_with(tag: &str, threshold: Option<Duration>) -> (u64, (u64, alloy_primitives::B256)) {
    let dir = scratch(tag);
    let net = Network::Apothem;
    let mut core = Core::open(net.spec(), &net.genesis().unwrap(), &dir.join("xone"), 0).unwrap();
    core.slow_block = threshold;
    for b in &vectors(N)[1..] {
        let (h, body) = parts(b);
        core.import(h, body, now()).unwrap();
    }
    (core.slow_blocks.load(Ordering::Relaxed), core.head())
}

#[test]
fn threshold_zero_logs_every_block_and_off_logs_none() {
    let (all, head) = import_with("slow-all", Some(Duration::ZERO));
    assert_eq!(all, N as u64);
    let (none, head_off) = import_with("slow-off", None);
    assert_eq!(none, 0);
    let (high, _) = import_with("slow-high", Some(Duration::from_secs(3600)));
    assert_eq!(high, 0);
    assert_eq!(head, head_off);
    assert_eq!(head.0, N as u64);
}
