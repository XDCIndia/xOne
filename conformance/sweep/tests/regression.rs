//! The named regression transactions: real Apothem transactions with their archive prestate,
//! receipt and state diff (`conformance/vectors/evm/regression/`, exported by `xone-sweep export`).
//! Each one is a lesson of issue 26 and must replay with no divergence.

use std::path::PathBuf;
use xone_sweep::{compare, mutants::mutant_of, replay, replay_with, Case};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/evm/regression")
}

fn run_named(prefix: &str) {
    let mut n = 0;
    for e in std::fs::read_dir(dir()).unwrap().flatten() {
        let f = e.file_name().into_string().unwrap();
        if !f.starts_with(&format!("{prefix}-")) {
            continue;
        }
        let c: Case = serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap();
        let o = replay(&c).unwrap_or_else(|e| panic!("{}: {e}", c.name));
        if let Some(d) = compare(&c, &o) {
            panic!("{} ({}): {d}", c.name, c.tx.hash);
        }
        // A case with a mutant must fail under it: otherwise the case proves nothing about the rule.
        if let Some(m) = mutant_of(&c.name) {
            let detected = replay_with(&c, &m).map_or(true, |o| compare(&c, &o).is_some());
            assert!(detected, "{}: its mutant replays clean", c.name);
        }
        n += 1;
    }
    assert!(n > 0, "no vector named {prefix}");
}

macro_rules! named {
    ($($test:ident => $prefix:literal),* $(,)?) => {
        $(#[test] fn $test() { run_named($prefix); })*
    };
}

named! {
    wedge_at_85197568 => "wedge-85197568",
    ecrecover_high_s_at_85220370 => "ecrecover-high-s-85220370",
    zero_length_memory_at_85220385 => "zero-length-memory-85220385",
    shr_at_35_4m => "shr-35.4M",
    sload_at_58_8m => "sload-58.8M",
    delegatecall_at_62_4m => "delegatecall-62.4M",
    staticcall_at_69_4m => "staticcall-69.4M",
    push0_at_64_0m => "push0-64.0M",
    create_at_62_2m => "create-62.2M",
    difficulty_at_56_4m => "difficulty-56.4M",
    prevrandao_at_68_7m => "prevrandao-68.7M",
}
