#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Mutation check of the reward and penalty engines: flip one rule in the source, run the one
//! test that is named for it in a scratch target directory, require it to fail, restore the file.
//! Slow (one build per mutant), so it only runs on request:
//!
//!     cargo test -p xone-consensus --test reward_mutants -- --ignored --nocapture

use std::{fs, path::PathBuf, process::Command};

/// (name, file under src, text that occurs once, the flip, test file, test that must fail)
const MUTANTS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "rewards read the post-transaction state",
        "reward/mod.rs",
        "if h.root != parent_root {",
        "if false {",
        "reward_penalty",
        "mutant_parent_state_root",
    ),
    (
        "sign txs counted out of order",
        "penalty/comeback.rs",
        "for &(number, hash) in blocks {",
        "for &(number, hash) in blocks.iter().rev() {",
        "reward_penalty",
        "mutant_out_of_order_scan_loses_a_sign_in_the_flagging_block",
    ),
    (
        "side blocks included",
        "chain.rs",
        "if h.hash != hash || h.number != number {",
        "if false {",
        "reward_penalty",
        "mutant_side_block_is_not_counted",
    ),
    (
        "penalty bounds fail open",
        "penalty/v2.rs",
        "let derived = penalties(spec, reader, input)?;",
        "let Ok(derived) = penalties(spec, reader, input) else { return Ok(()) };",
        "reward_penalty",
        "mutant_penalty_bounds_fail_open",
    ),
    (
        "missing body returns zero",
        "chain.rs",
        "r.sign_txs(hash, number).ok_or(Error::Missing {\n        what: \"body\",\n        number,\n        hash,\n    })",
        "Ok(r.sign_txs(hash, number).unwrap_or_default())",
        "reward_penalty",
        "mutant_missing_body_is_not_zero_signatures",
    ),
    (
        "pre-upgrade LimitPenaltyEpoch used after the upgrade",
        "penalty/v2.rs",
        "let limit = post_upgrade_limit(cfg.limit_penalty_epoch);",
        "let limit = LIMIT_PENALTY_EPOCH_V2;",
        "reward_penalty",
        "mutant_limit_penalty_epoch_of_the_pre_upgrade_rule",
    ),
    (
        "V2 switch block plus one is not special",
        "penalty/v2.rs",
        "if number == v2.switch_block + 1 {",
        "if false {",
        "reward_penalty",
        "mutant_switch_plus_one_special_case",
    ),
    (
        "proportional reward disabled (payout)",
        "reward/tiered.rs",
        "let proportional = spec.tip_proportional_reward.is_active(number);",
        "let proportional = false;",
        "reward_vectors",
        "mutant_proportional_reward_at_84_733_000",
    ),
    (
        "proportional reward disabled (rule)",
        "reward/mod.rs",
        "let proportional = spec.tip_proportional_reward.is_active(number);",
        "let proportional = false;",
        "reward_penalty",
        "rule_follows_the_fork_rows",
    ),
    (
        "payout multiplies before it divides",
        "reward/v1.rs",
        ".map(|(&a, &n)| (a, per * U256::from(n)))",
        ".map(|(&a, &n)| (a, chain_reward * U256::from(n) / U256::from(counts.total)))",
        "reward_penalty",
        "flat_payout_divides_before_it_multiplies",
    ),
    (
        "V1 owner-is-foundation skip removed",
        "reward/mod.rs",
        "if mode == Split::Upgrade || owner != foundation {",
        "if true {",
        "reward_penalty",
        "v1_owner_that_is_the_foundation_gets_no_owner_share_but_tiered_does",
    ),
];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Puts the original text back, also when the test panics.
struct Restore(PathBuf, String);
impl Drop for Restore {
    fn drop(&mut self) {
        fs::write(&self.0, &self.1).unwrap();
    }
}

#[test]
#[ignore = "builds the crate once per mutant"]
fn the_tests_catch_every_mutant() {
    let target = crate_dir().join("../../target/reward-mutants");
    let mut survivors = vec![];
    for (name, file, from, to, test_file, test) in MUTANTS {
        let path = crate_dir().join("src").join(file);
        let original = fs::read_to_string(&path).unwrap();
        assert_eq!(
            original.matches(from).count(),
            1,
            "`{from}` must occur once in {file}"
        );
        let guard = Restore(path.clone(), original.clone());
        fs::write(&path, original.replace(from, to)).unwrap();
        let out = Command::new(env!("CARGO"))
            .args([
                "test",
                "--offline",
                "--locked",
                "-p",
                "xone-consensus",
                "--test",
                test_file,
                test,
            ])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(crate_dir())
            .output()
            .unwrap();
        drop(guard);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "{file} was not restored"
        );
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        // Caught: it built and the named test ran and failed (not a build error).
        let caught = !out.status.success() && text.contains(&format!("test {test} ... FAILED"));
        println!(
            "mutant `{name}` ({file}): {}  [{test_file}::{test}]",
            if caught { "CAUGHT" } else { "SURVIVED" }
        );
        if !caught {
            survivors.push(format!("{name}: {text}"));
        }
        // And the unmutated source passes that test (checked in the main run, not here).
    }
    assert!(
        survivors.is_empty(),
        "mutants the tests did not catch:\n{}",
        survivors.join("\n")
    );
}
