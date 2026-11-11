#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! Replay of the committed Apothem window around the V2 switch (`conformance/vectors/replay/`):
//! the state of block 56,828,700 (the V1 to V2 switch block, a checkpoint; the state is the one the
//! runner verified against the archive's header roots on the way from the DIAGNOSTIC-PIVOT at
//! 56,826,900, and its root is checked against the header again here), then the blocks above it
//! through the first V2 epoch switch, the gap block of the first V2 epoch and the second V2 epoch
//! switch, which pays the first V2 reward. Mutants that must be caught at the block they break.

use alloy_primitives::B256;
use std::path::PathBuf;
use xone_chainspec::{ChainSpec, Network};
use xone_harness::{
    cache::{self, decode, Block},
    pivot,
    replay::{run_from, Opts, Report},
};

const PIVOT: u64 = 56_828_700;
/// The first V2 epoch switch (the block after the V2 switch block), the gap block of the epoch that
/// follows, and the second V2 epoch switch.
const FIRST_SWITCH: u64 = 56_828_701;
const GAP: u64 = 56_829_150;
const SECOND_SWITCH: u64 = SECOND;
const SECOND: u64 = 0; // set from the vectors
const LAST: u64 = SECOND_SWITCH + 1;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/replay")
}

fn blocks() -> Vec<Block> {
    cache::read(&dir().join("apothem-v2-56827800-56829700.jsonl.gz"))
        .unwrap()
        .iter()
        .map(|l| decode(l).unwrap())
        .collect()
}

fn replay(spec: &ChainSpec, gap_offset: u64, to: u64) -> Report {
    let blocks = blocks();
    let head = blocks.iter().find(|b| b.n == PIVOT).unwrap();
    let start = pivot::start(
        Network::Apothem,
        &dir().join("apothem-state-56828700.json.gz"),
        PIVOT,
        head.header.state_root,
        B256::ZERO,
    )
    .unwrap();
    let opts = Opts {
        to,
        state_dir: None,
        every: 0,
        resume: false,
        progress: 0,
        gap_offset,
        dump_state: None,
    };
    run_from(spec, start, &blocks, &opts).unwrap()
}

#[test]
fn the_first_two_v2_epochs_replay_with_no_mismatch() {
    let r = replay(&Network::Apothem.spec(), 0, LAST);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!(r.last_ok, LAST);
    assert_eq!(r.v2_switches, 2, "the first block and the second epoch switch");
    assert_eq!(r.gap_snapshots, 1, "the gap block {GAP}");
    assert!(r.committed_count > 0 && r.committed.is_some());
}

fn diverges_at(r: Report, block: u64) {
    let d = r.divergence.expect("the mutant must be caught");
    assert_eq!(d.block, block, "{d:?}");
    assert_eq!(r.last_ok, block - 1);
}

/// Mutant: the second epoch switch derives its lists from the gap snapshot of the epoch before,
/// which the runner never built (the V1 era has no gap snapshot): the switch is refused.
#[test]
fn mutant_gap_snapshot_of_the_wrong_block_is_caught_at_the_second_switch() {
    diverges_at(replay(&Network::Apothem.spec(), 900, LAST), SECOND_SWITCH);
}

/// Mutant: no V2 reward. The first V2 reward is paid at the second epoch switch.
#[test]
fn mutant_v2_reward_off_is_caught_at_the_second_switch() {
    let mut spec = Network::Apothem.spec();
    spec.xdpos.reward = 0;
    let r = replay(&spec, 0, LAST);
    assert!(r.divergence.as_ref().unwrap().mismatches.iter().any(|m| m.field == "stateRoot"));
    diverges_at(r, SECOND_SWITCH);
}
