//! The V2 BFT simulation over many seeds (`sim/README.md`): safety invariants on every step, liveness
//! after the declared delivery bound. The release run is the gate:
//!
//!     cargo test -p xone-sim --release --test bft_seeds -- --nocapture
//!
//! `SIM_SEEDS` overrides the count of seeds (a debug run takes 300 by default).

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use xone_consensus::v2::cert::min_signers;
use xone_sim::net::{run, scenario, Stats};

const SEEDS: u64 = if cfg!(debug_assertions) { 300 } else { 10_000 };

fn seeds() -> u64 {
    // ponytail: the test reads its own knob; the rule crates never read the environment
    std::env::var("SIM_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS)
}

/// Runs `0..n` on all cores; returns the totals or the first failure by seed.
fn sweep(n: u64) -> Result<(Stats, u64, u64, f64, u64), String> {
    let next = AtomicU64::new(0);
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let results = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut total = Stats::default();
                    let (mut worst, mut byz_seeds, mut ratio, mut recovered) =
                        (0u64, 0u64, 0f64, 0u64);
                    let mut err: Option<(u64, String)> = None;
                    loop {
                        let seed = next.fetch_add(1, Ordering::Relaxed);
                        if seed >= n {
                            break;
                        }
                        match run(seed) {
                            Ok(r) => {
                                total.add(&r.stats);
                                byz_seeds += u64::from(r.byz > 0);
                                recovered += 1;
                                let took = r.recovery.unwrap_or(0);
                                worst = worst.max(took);
                                ratio = ratio.max(took as f64 / r.bound as f64);
                            }
                            Err(e) => {
                                if err.as_ref().is_none_or(|(s, _)| seed < *s) {
                                    err = Some((seed, e));
                                }
                                break;
                            }
                        }
                    }
                    (total, worst, byz_seeds, ratio, recovered, err)
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut total = Stats::default();
    let (mut worst, mut byz, mut ratio, mut ok) = (0, 0, 0f64, 0);
    let mut err: Option<(u64, String)> = None;
    for (t, w, b, r, k, e) in results {
        total.add(&t);
        worst = worst.max(w);
        byz += b;
        ratio = ratio.max(r);
        ok += k;
        if let Some((s, m)) = e {
            if err.as_ref().is_none_or(|(x, _)| s < *x) {
                err = Some((s, m));
            }
        }
    }
    match err {
        Some((_, m)) => Err(m),
        None => {
            assert_eq!(ok, n);
            Ok((total, worst, byz, ratio, ok))
        }
    }
}

#[test]
fn no_safety_violation_and_recovery_within_the_bound_in_every_seed() {
    let n = seeds();
    let t = Instant::now();
    let (s, worst, byz, ratio, ok) = match sweep(n) {
        Ok(x) => x,
        Err(e) => panic!("{e}"),
    };
    eprintln!(
        "{ok} seeds in {:?} ({byz} with Byzantine validators); worst recovery {worst} ticks, at most {:.0}% of its bound\n{s:?}",
        t.elapsed(),
        ratio * 100.0
    );
    // the run is not vacuous: every mechanism under test happened
    assert!(
        s.commits > 0 && s.votes > 0 && s.timeouts > 0 && s.sync_infos > 0 && s.sibling_votes > 0
    );
    assert!(s.kills.iter().all(|k| *k > 0) && s.restarts > 0 && s.dropped > 0 && s.refused > 0);
    assert!(
        s.byz_variants > 0
            && s.byz_equivocations > 0
            && s.byz_lock_attacks > 0
            && s.byz_forged_sync > 0
    );
    assert!(s.scored_invalid > 0 && s.scored_oversized > 0);
}

/// The quorum of the config threshold 0.667 for 4 to 14 validators: two quorums meet in more than f
/// validators (so in an honest one), and the honest validators alone are a quorum.
#[test]
fn the_quorum_of_every_validator_count_is_safe_and_reachable_with_f_faulty() {
    for n in 4..=14usize {
        let q = min_signers(n, 0.667).unwrap();
        let f = (n - 1) / 3;
        assert!(
            2 * q > n + f,
            "n {n}: two quorums of {q} meet in {} validators, f is {f}",
            2 * q - n
        );
        assert!(
            n - f >= q,
            "n {n}: {} honest validators cannot make a quorum of {q}",
            n - f
        );
    }
}

/// The seeds cover every validator count with every number of Byzantine validators up to f.
#[test]
fn the_seeds_cover_every_size_and_every_fault_count() {
    let mut seen = std::collections::BTreeSet::new();
    for seed in 0..3_000 {
        let s = scenario(seed);
        assert!(s.byz.len() <= (s.n - 1) / 3 && (4..=14).contains(&s.n));
        seen.insert((s.n, s.byz.len()));
    }
    for n in 4..=14usize {
        for b in 0..=(n - 1) / 3 {
            assert!(
                seen.contains(&(n, b)),
                "no seed with n {n} and {b} Byzantine validators"
            );
        }
    }
}
