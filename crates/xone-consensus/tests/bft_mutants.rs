#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Mutation check of the V2 BFT machine: change one rule at a time in the source, run the tests that must
//! notice (the vectors and the machine tests of this crate, or the simulator), require them to fail, restore
//! the files. One build per mutant, so it runs on request:
//!
//!     cargo test -p xone-consensus --test bft_mutants -- --ignored --nocapture
//!
//! A mutant may change several files (the lock check is in the voting rule and again in the safety
//! unit; both go for the simulator). The simulator runs in a debug build with its default 300 seeds.

use std::{fs, path::PathBuf, process::Command};

type Edit = (&'static str, &'static str, &'static str);

/// (name, edits as (file under src, text that occurs once, replacement), package, test target that must fail).
const MUTANTS: &[(&str, &[Edit], &str, &str)] = &[
    ("voting rule: the lock check is dropped", &[("v2/bft/rule.rs", "        Ok(Verdict::No(Why::NotExtended))", "        Ok(Verdict::Vote { extends_lock: true })")], "xone-consensus", "bft_vectors"),
    ("voting rule: a vote in a round already voted", &[("v2/bft/rule.rs", "    if round <= highest_voted_round {", "    if false {")], "xone-consensus", "bft_vectors"),
    ("voting rule: the block's round is not compared", &[("v2/bft/rule.rs", "    if block.round != round {", "    if false {")], "xone-consensus", "bft_vectors"),
    ("voting rule: a QC round equal to the lock's passes", &[("v2/bft/rule.rs", "    if qc_round > lock.round {", "    if qc_round >= lock.round {")], "xone-consensus", "bft_vectors"),
    ("pool: a signer's second message is kept", &[("v2/pool.rs", "        if entries.iter().any(|(a, _)| *a == signer) {", "        if false {")], "xone-consensus", "bft_machine"),
    ("syncinfo: applied before it is verified", &[("v2/syncinfo.rs", "    if let Err(e) = verify(m, c, qc, tc) {", "    let _ = m.process_qc(c, qc, fx);\n    if let Err(e) = verify(m, c, qc, tc) {")], "xone-consensus", "bft_machine"),
    ("syncinfo: the TC is not verified", &[("v2/syncinfo.rs", "    verify_sync_info_tc_with(&m.scheme, tc, &ctx)\n}", "    let _ = (&m.scheme, tc, &ctx);\n    Ok(())\n}")], "xone-consensus", "bft_machine"),
    ("syncinfo: no cap on the signatures", &[("v2/syncinfo.rs", "(n > MAX_PEER_SIGNATURES).then_some(", "(n > 1000 * MAX_PEER_SIGNATURES).then_some(")], "xone-consensus", "bft_machine"),
    ("timer: the exponent is not capped", &[("v2/timer.rs", "diff.min(u64::from(self.max_exp))", "diff")], "xone-consensus", "bft_vectors"),
    ("timer: a base below 1 replaces the base", &[("v2/timer.rs", "        if row.exp_timeout.base >= 1.0 {", "        if true {")], "xone-consensus", "bft_vectors"),
    ("timeout: the periodic SyncInfo is one timeout late", &[("v2/bft/timeout.rs", "m.timeout_count.checked_rem(t) == Some(0)", "m.timeout_count.checked_rem(t) == Some(1)")], "xone-consensus", "bft_machine"),
    ("timeout: an old timer token is accepted", &[("v2/bft/timeout.rs", "    if token != m.timer.token() {", "    if false {")], "xone-consensus", "bft_machine"),
    ("round: the mine period is not the config's", &[("v2/bft/mod.rs", "        self.timer.set_params(row);\n        let mine_period = row.mine_period;", "        self.timer.set_params(row);\n        let mine_period = 2;")], "xone-consensus", "bft_machine"),
    ("forensics: the id has its QCs in the other order", &[("v2/forensics.rs", "format!(\"{diverging:#x}:{:#x}:{:#x}\", qc1.hash, qc2.hash)", "format!(\"{diverging:#x}:{:#x}:{:#x}\", qc2.hash, qc1.hash)")], "xone-consensus", "bft_vectors"),
    (
        "highest QC: the placeholder QC is kept until a QC of a higher round",
        &[("v2/bft/mod.rs", "let unset = self.highest_qc.proposed_block_info.hash.is_zero();", "let unset = false;")],
        "xone-consensus",
        "bft_machine",
    ),
    (
        "highest QC: the QC is not handed to the driver to store",
        &[("v2/bft/mod.rs", "            self.rec.observe_qc(block_ref(info), None);\n            fx.push(Effect::Store(syncinfo::originate(self)));\n", "            self.rec.observe_qc(block_ref(info), None);\n")],
        "xone-consensus",
        "bft_machine",
    ),
    (
        "highest TC: it is not handed to the driver to store",
        &[("v2/bft/mod.rs", "            self.rec.observe_tc(tc.round);\n            fx.push(Effect::Store(syncinfo::originate(self)));\n", "            self.rec.observe_tc(tc.round);\n")],
        "xone-consensus",
        "bft_machine",
    ),
    // the simulator
    ("sim: the lock check is dropped (rule and safety unit)", &[("v2/bft/rule.rs", "        Ok(Verdict::No(Why::NotExtended))", "        Ok(Verdict::Vote { extends_lock: true })"), ("v2/safety.rs", "                if !(i.ctx.qc_round > l.round || i.ctx.extends_lock) {", "                if false {")], "xone-sim", "bft_seeds"),
    ("sim: duplicate votes are counted", &[("v2/pool.rs", "        if entries.iter().any(|(a, _)| *a == signer) {", "        if false {")], "xone-sim", "bft_seeds"),
    ("sim: send before persist", &[("v2/bft/mod.rs", "                if next != self.rec {\n                    fx.push(Effect::PersistIntent {\n                        intent,\n                        record: next.clone(),\n                    });\n                    self.rec = next;\n                }\n                fx.push(Effect::Sign(intent));\n                fx.push(Effect::Send(out));", "                fx.push(Effect::Sign(intent));\n                fx.push(Effect::Send(out));\n                if next != self.rec {\n                    fx.push(Effect::PersistIntent {\n                        intent,\n                        record: next.clone(),\n                    });\n                    self.rec = next;\n                }")], "xone-sim", "bft_seeds"),
    ("sim: a forged SyncInfo is accepted", &[("v2/syncinfo.rs", "    if let Err(e) = verify(m, c, qc, tc) {", "    let checked = verify(m, c, qc, tc);\n    let checked: Result<(), Error> = if checked.is_err() { Ok(()) } else { checked };\n    if let Err(e) = checked {")], "xone-sim", "bft_seeds"),
];

struct Restore(Vec<(PathBuf, String)>);
impl Drop for Restore {
    fn drop(&mut self) {
        for (p, s) in &self.0 {
            fs::write(p, s).unwrap();
        }
    }
}

#[test]
#[ignore = "builds the crate once per mutant"]
fn the_bft_tests_and_the_simulator_catch_every_mutant() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("../../target/bft-mutants");
    let mut survivors = vec![];
    // ponytail: `MUTANT_FILTER=<text>` runs the mutants whose name has the text (test code only)
    let filter = std::env::var("MUTANT_FILTER").unwrap_or_default();
    for (name, edits, package, test) in MUTANTS.iter().filter(|m| m.0.contains(&filter)) {
        let mut saved = vec![];
        let mut files: Vec<(PathBuf, String)> = vec![];
        for (file, from, to) in *edits {
            let path = root.join("src").join(file);
            let current = match files.iter().find(|(p, _)| *p == path) {
                Some((_, text)) => text.clone(),
                None => {
                    let original = fs::read_to_string(&path).unwrap();
                    saved.push((path.clone(), original.clone()));
                    original
                }
            };
            assert_eq!(
                current.matches(from).count(),
                1,
                "`{from}` must occur once in {file}"
            );
            files.retain(|(p, _)| *p != path);
            files.push((path, current.replace(from, to)));
        }
        let guard = Restore(saved);
        for (p, text) in &files {
            fs::write(p, text).unwrap();
        }
        let out = Command::new(env!("CARGO"))
            .args([
                "test",
                "--offline",
                "--locked",
                "-p",
                package,
                "--test",
                test,
            ])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(&root)
            .output()
            .unwrap();
        drop(guard);
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        let compiled = !text.contains("could not compile");
        let caught = compiled
            && !out.status.success()
            && (text.contains("test result: FAILED") || text.contains("panicked"));
        let by: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("test ") && l.ends_with("FAILED"))
            .collect();
        let first = text
            .lines()
            .find(|l| l.contains("panicked") || l.starts_with("seed "))
            .unwrap_or("");
        println!(
            "mutant {name} [{package} {test}]: {} {by:?} {first}",
            if caught {
                "CAUGHT"
            } else if !compiled {
                "DID NOT COMPILE"
            } else {
                "SURVIVED"
            }
        );
        if !caught {
            survivors.push(format!("{name} ({package} {test})"));
        }
    }
    assert!(
        survivors.is_empty(),
        "not caught:\n{}",
        survivors.join("\n")
    );
}
