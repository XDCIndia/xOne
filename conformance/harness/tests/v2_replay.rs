#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! Replay of the Apothem window around the V2 switch. The blocks are committed
//! (`conformance/vectors/replay/apothem-v2-56827800-56830294.jsonl.gz`, 2.5 MB: 56,827,800 to
//! 56,830,294). The start state is not: the Apothem state at 56,828,700 is 5.3 million slots, 294 MB
//! gzipped, and the part the window reads or writes is still 143 MB with its code. The tests read it
//! from `conformance/harness/cache/pivot-apothem-56828700.json.gz` and are `#[ignore]` without it;
//! make it with `xone-replay pivot --block 56826900 --dump-state PATH --dump-block 56828700 ...`
//! (the archive, about an hour of downloads) and run `cargo test -p xone-harness --test v2_replay
//! -- --ignored`. The state is the one the runner verified against the archive's header roots
//! (DIAGNOSTIC-PIVOT at 56,826,900, then 1,800 blocks of its own execution), and its root is checked
//! against the header again at the start. The blocks above it
//! through the first V2 epoch switch, the gap block of the first V2 epoch and the second V2 epoch
//! switch (56,829,393), which pays the first V2 reward. The state keeps the slots of the accounts
//! the window writes or reads and the storage root only of the others (`pivot::prune`). Mutants that must be caught at the block they break.

use alloy_primitives::B256;
use std::path::PathBuf;
use xone_chainspec::{ChainSpec, Network};
use xone_harness::{
    cache::{self, decode, Block},
    pivot,
    replay::{run_from, Opts, Report},
};

const PIVOT: u64 = 56_828_700;
/// The first V2 epoch switch is the block after the V2 switch block (56,828,701); then the gap
/// block of the epoch that follows, and the second V2 epoch switch.
const SECOND_SWITCH: u64 = 56_829_393;
/// The first switch that reads a gap snapshot built from state: its epoch has a V2 gap block (56,829,150).
const THIRD_SWITCH: u64 = 56_830_293;
const LAST: u64 = THIRD_SWITCH + 1;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/replay")
}

fn blocks() -> Vec<Block> {
    cache::read(&dir().join("apothem-v2-56827800-56830294.jsonl.gz"))
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
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cache/pivot-apothem-56828700.json.gz"),
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
#[ignore = "needs the pivot state cache, see the module docs"]
fn the_first_three_v2_epoch_switches_replay_with_no_mismatch() {
    let r = replay(&Network::Apothem.spec(), 0, LAST);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!(r.last_ok, LAST);
    assert_eq!(r.v2_switches, 3);
    assert_eq!(
        r.gap_snapshots, 2,
        "the gap blocks 56,829,150 and 56,830,050"
    );
    assert!(r.committed_count > 0 && r.committed.is_some());
}

fn diverges_at(r: Report, block: u64) {
    let d = r.divergence.expect("the mutant must be caught");
    assert_eq!(d.block, block, "{d:?}");
    assert_eq!(r.last_ok, block - 1);
}

/// Mutant: the third epoch switch derives its lists from the gap snapshot of the epoch before,
/// which the runner never built (the V1 era has no gap snapshot): the switch is refused. The second
/// switch is still in the first V2 epoch, whose pool is the V1 list, not a gap snapshot.
#[test]
#[ignore = "needs the pivot state cache, see the module docs"]
fn mutant_gap_snapshot_of_the_wrong_block_is_caught_at_the_third_switch() {
    diverges_at(replay(&Network::Apothem.spec(), 900, LAST), THIRD_SWITCH);
}

/// Mutant: no V2 reward. The first V2 reward is paid at the second epoch switch.
#[test]
#[ignore = "needs the pivot state cache, see the module docs"]
fn mutant_v2_reward_off_is_caught_at_the_second_switch() {
    let mut spec = Network::Apothem.spec();
    spec.xdpos.reward = 0;
    let r = replay(&spec, 0, LAST);
    assert!(r
        .divergence
        .as_ref()
        .unwrap()
        .mismatches
        .iter()
        .any(|m| m.field == "stateRoot"));
    diverges_at(r, SECOND_SWITCH);
}
