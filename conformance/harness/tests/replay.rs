#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! Sequential replay of the committed Apothem vectors (`conformance/vectors/replay/`, blocks 0 to
//! 1,800 as the archive returned them) on xOne's own state, and mutants that must be caught.

use std::path::Path;
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_harness::{
    cache::{self, decode, Block},
    replay::{run, Opts, Report},
};

fn blocks(to: u64) -> Vec<Block> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/replay/apothem-0-1800.jsonl.gz");
    cache::read(&p)
        .unwrap()
        .iter()
        .take(to as usize + 1)
        .map(|l| decode(l).unwrap())
        .collect()
}

fn replay(spec: &ChainSpec, to: u64) -> Report {
    let opts = Opts {
        to,
        state_dir: None,
        every: 0,
        resume: false,
        keep: false,
        progress: 0,
        gap_offset: 0,
        dump_state: None,
    };
    run(Network::Apothem, spec, &blocks(to), &opts).unwrap()
}

#[test]
fn first_200_blocks_replay_with_no_mismatch() {
    let r = replay(&Network::Apothem.spec(), 200);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!((r.last_ok, r.blocks), (200, 200));
}

/// Through the first V1 checkpoint reward (block 1,800, paid for blocks 1 to 900) and every block
/// that carries a sign transaction run in the EVM.
#[test]
fn blocks_1_to_1800_replay_with_no_mismatch() {
    let r = replay(&Network::Apothem.spec(), 1800);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!((r.last_ok, r.blocks), (1800, 1800));
    assert_eq!(
        r.archive_hash_checks, 18,
        "every 100th block hash is the archive's own"
    );
}

fn diverges_at(spec: &ChainSpec, to: u64, block: u64, field: &str) {
    let r = replay(spec, to);
    let d = r.divergence.expect("the mutant must be caught");
    assert_eq!(d.block, block, "{d:?}");
    assert!(d.mismatches.iter().any(|m| m.field == field), "{d:?}");
    assert_eq!(r.last_ok, block - 1);
}

/// Mutant: checkpoint reward off. The first V1 reward is at block 1,800.
#[test]
fn mutant_checkpoint_reward_off_is_caught_at_1800() {
    let mut spec = Network::Apothem.spec();
    spec.xdpos.reward = 0;
    diverges_at(&spec, 1800, 1800, "stateRoot");
}

/// Mutant: `TIPSigning` at block 1 instead of 3,000,000. That turns on both of its rules, the
/// wipe of contract 0x89 at the fork block and the free no-EVM route of sign transactions (block
/// 16 holds the first one, which costs gas in the archive). The wipe is the first to differ.
#[test]
fn mutant_sign_tx_free_is_caught_at_the_tip_signing_block() {
    let mut spec = Network::Apothem.spec();
    spec.effective.tip_signing = ForkBlock::At(1);
    diverges_at(&spec, 100, 1, "stateRoot");
}
