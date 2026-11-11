#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! Mutation check of the V2 follower rules: flip one rule at a time in the source, run the V2 tests in
//! a scratch target directory, require them to fail, restore the file. One build per mutant, so it runs
//! on request:
//!
//!     cargo test -p xone-consensus --test v2_mutants -- --ignored --nocapture

use std::{fs, path::PathBuf, process::Command};

/// (name, file under src, text that occurs once, replacement, test target that must fail).
const MUTANTS: &[(&str, &str, &str, &str, &str)] = &[
    ("threshold rounding (floor instead of the float comparison)", "v2/cert.rs", "f64::from(k) < need", "f64::from(k + 1) <= need", "v2_vectors"),
    ("duplicate signer counted", "v2/cert.rs", "check(signers.len(), masternodes.len(), threshold)?;", "check(qc.signatures.len(), masternodes.len(), threshold)?;", "v2_vectors"),
    ("duplicate signer counted (TC)", "v2/cert.rs", "check(signers.len(), ctx.masternodes.len(), ctx.threshold)", "check(tc.signatures.len(), ctx.masternodes.len(), ctx.threshold)", "v2_vectors"),
    ("non-member counted", "v2/cert.rs", "if !masternodes.contains(&who) {", "if false {", "v2_vectors"),
    ("high-S signature counted", "v2/cert.rs", ".is_some_and(|s| s.normalize_s().is_none())", ".is_some()", "v2_vectors"),
    ("blank TC accepted everywhere", "v2/cert.rs", "    if ctx.snapshot_candidates == 0 {\n        return Err(Error::SnapshotEmpty);", "    if is_blank_tc(tc) {\n        return Ok(());\n    }\n    if ctx.snapshot_candidates == 0 {\n        return Err(Error::SnapshotEmpty);", "v2_vectors"),
    ("blank TC rejected in SyncInfo", "v2/cert.rs", "    if is_blank_tc(tc) {\n        return Ok(());\n    }\n    verify_tc_with(scheme, tc, ctx)", "    verify_tc_with(scheme, tc, ctx)", "v2_vectors"),
    ("QC of round 0 held to the threshold", "v2/cert.rs", "if qc.proposed_block_info.round > 0 {", "if true {", "v2_vectors"),
    ("epoch switch detected by block number", "v2/epoch_switch.rs", "Ok((qc.round < start, num))", "Ok((number % epoch == 0, num))", "v2_vectors"),
    ("epoch switch at round misses the V2 switch block as parent", "v2/epoch_switch.rs", "if parent_number == v2.switch_block {", "if false {", "v2_vectors"),
    ("cap not applied", "v2/masternodes.rs", ".filter(|a| !penalties.contains(a))\n        .take(max)", ".filter(|a| !penalties.contains(a))\n        .take(max.max(usize::MAX))", "v2_vectors"),
    ("cap applied before the penalties", "v2/masternodes.rs", ".filter(|a| !penalties.contains(a))\n        .take(max)\n        .copied()", ".take(max)\n        .filter(|a| !penalties.contains(a))\n        .copied()", "v2_vectors"),
    ("leader index ignores the epoch", "v2/masternodes.rs", ".checked_rem(epoch)\n        .ok_or(Error::ZeroEpoch)?\n        .checked_rem(n)", ".checked_add(0)\n        .ok_or(Error::ZeroEpoch)?\n        .checked_rem(n)", "v2_vectors"),
    ("standby pool ignored", "v2/gap_snapshot.rs", ".filter(|a| !masternodes.contains(a) && !penalties.contains(a))", ".filter(|_| false)", "v2_vectors"),
    ("standby pool keeps the penalised", "v2/gap_snapshot.rs", ".filter(|a| !masternodes.contains(a) && !penalties.contains(a))", ".filter(|a| !masternodes.contains(a))", "v2_vectors"),
    ("frozen sort replaced by a strict comparison", "v2/gap_snapshot.rs", "sort_slice(&mut pool, |a, b| a.1 >= b.1);", "sort_slice(&mut pool, |a, b| a.1 > b.1);", "v2_vectors"),
    ("first V2 epoch pool read from state", "v2/gap_snapshot.rs", "Ok(number - r == switch_block)", "Ok(number == switch_block + 1)", "v2_vectors"),
    ("commit without the grandparent round check", "v2/commit.rs", "if proposed.round.wrapping_sub(2) != grand.round {", "if false {", "v2_vectors"),
    ("commit again below the highest committed", "v2/commit.rs", "h.round >= grand.round || h.number >= grand.number", "h.round > grand.round", "v2_vectors"),
    ("minePeriod not enforced", "verify/header.rs", "&& parent.timestamp.wrapping_add(mine_period) > h.timestamp", "&& false", "v2_header"),
    ("derived lists ignored", "verify/header.rs", "(masternodes, penalties) = (want.validators, want.penalties);", "let _ = want;", "v2_header"),
];

struct Restore(PathBuf, String);
impl Drop for Restore {
    fn drop(&mut self) {
        fs::write(&self.0, &self.1).unwrap();
    }
}

#[test]
#[ignore = "builds the crate once per mutant"]
fn the_v2_tests_catch_every_mutant() {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/v2-mutants");
    let mut survivors = vec![];
    for (name, file, from, to, test) in MUTANTS {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(file);
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
                test,
            ])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
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
        let compiled = !text.contains("could not compile");
        let caught = compiled && !out.status.success() && text.contains("test result: FAILED");
        let by: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("test ") && l.ends_with("FAILED"))
            .collect();
        println!(
            "mutant {name}: {} {by:?}",
            if caught {
                "CAUGHT"
            } else if !compiled {
                "DID NOT COMPILE"
            } else {
                "SURVIVED"
            }
        );
        if !caught {
            survivors.push(format!("{name} ({file})"));
        }
    }
    assert!(
        survivors.is_empty(),
        "not caught:\n{}",
        survivors.join("\n")
    );
}
