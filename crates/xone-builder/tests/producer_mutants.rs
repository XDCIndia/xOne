#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the sources are trusted inputs and a panic is the failure signal"
)]
//! Mutation check of the producer (XB-23): change one rule at a time in the source, run the test that must
//! notice, require it to fail, restore the files. One build per mutant, so it runs on request:
//!
//!     PRODUCER_MUTANT_TARGET=<scratch dir> cargo test -p xone-builder --test producer_mutants -- --ignored --nocapture
//!
//! `MUTANT_FILTER=<text>` runs the mutants whose name has the text.

use std::{fs, path::PathBuf, process::Command};

/// (file under `crates/`, text that occurs once, replacement).
type Edit = (&'static str, &'static str, &'static str);

const B: &str = "xone-builder/src/producer/";
const U: &str = "xone-consensus/src/unit.rs";

/// (name, edits, package, test target that must fail).
const MUTANTS: &[(&str, &[Edit], &str, &str)] = &[
    (
        "double-mining a round: the mined-round guard of the schedule is off",
        &[("xone-builder/src/producer/schedule.rs", "    if round <= highest {", "    if false {")],
        "xone-builder",
        "producer_sim",
    ),
    (
        "double-mining a round: the safety record is ignored by the unit",
        &[(U, "        let mut next = decide(&self.rec, &intent, self.dur)?;", "        let mut next = self.rec.clone();\n        next.signed[2].round = intent.round;")],
        "xone-builder",
        "producer_sim",
    ),
    (
        "double-mining a round: the safety record is ignored (unit tests)",
        &[(U, "        let mut next = decide(&self.rec, &intent, self.dur)?;", "        let mut next = self.rec.clone();\n        next.signed[2].round = intent.round;")],
        "xone-consensus",
        "unit",
    ),
    (
        "signing before persisting",
        &[
            (U, "        if next != self.rec {\n            // the write and the sync come first", "        let early = self.backend.sign(&Authorized { digest: &intent.digest, domain });\n        if next != self.rec {\n            // the write and the sync come first"),
            (U, "        self.backend.sign(&auth)\n    }\n}\n\nimpl<St: SafetyStore, B: Backend> Sealer for", "        let _ = auth;\n        early\n    }\n}\n\nimpl<St: SafetyStore, B: Backend> Sealer for"),
        ],
        "xone-consensus",
        "unit",
    ),
    (
        "proposing out of turn (V2): any masternode may lead",
        &[("xone-builder/src/producer/schedule.rs", "        Ok(l) if l == me => Ok(()),", "        Ok(_) => Ok(()),")],
        "xone-builder",
        "producer_schedule",
    ),
    (
        "proposing out of turn (V1): the wait of an out-of-turn masternode is not kept",
        &[("xone-builder/src/producer/schedule.rs", "        V1Slot::OutOfTurn { wait } => now.saturating_sub(parent_time) >= wait,", "        V1Slot::OutOfTurn { .. } => true,")],
        "xone-builder",
        "producer_schedule",
    ),
    (
        "wrong gas-limit tier: the dynamic row is read at the parent's number",
        &[("xone-builder/src/producer/gaslimit.rs", "        row if row.is_active(number) =>", "        row if row.is_active(number.saturating_sub(1)) =>")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "wrong gas-limit tier: the plateau rule where the row is unset aims at the ceiling",
        &[("xone-builder/src/producer/gaslimit.rs", "        ForkBlock::Never => calc_gas_limit(parent, plateau_ceil(parent)),", "        ForkBlock::Never => calc_gas_limit(parent, ceil),")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "wrong gas-limit tier: the base fee tier of the parent",
        &[("xone-builder/src/producer/gaslimit.rs", "    xone_evm::basefee::base_fee_for_block(spec, number)", "    xone_evm::basefee::base_fee_for_block(spec, number.saturating_sub(1))")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "sign tx with gas charged: a price above zero",
        &[("xone-builder/src/producer/signtx.rs", "        gas_price: 0,", "        gas_price: 1,")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "sign tx with the wrong gas",
        &[("xone-builder/src/producer/signtx.rs", "pub const GAS: u64 = 200_000;", "pub const GAS: u64 = 100_000;")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "sign tx for every block: the 15 block cadence is off",
        &[("xone-builder/src/producer/signtx.rs", "    number % MERGE_SIGN_RANGE == 0 || !spec.effective.tip2019.is_active(number)", "    true")],
        "xone-builder",
        "producer_vectors",
    ),
    (
        "special transactions are not first",
        &[("xone-builder/src/producer/build.rs", "    f.run(special, None);\n    f.run(normal, p.base_fee);", "    f.run(normal, p.base_fee);\n    f.run(special, None);")],
        "xone-builder",
        "producer_select",
    ),
    (
        "V1 seal: the creator never is its own M2",
        &[("xone-builder/src/producer/seal_v1.rs", "        if m2 == me {", "        if false {")],
        "xone-builder",
        "producer_construction",
    ),
    (
        "V2 prepare: the QC is not the highest QC",
        &[("xone-builder/src/producer/seal_v2.rs", "        extra_data: ExtraFieldsV2 { round: p.round, quorum_cert: p.qc.clone() }", "        extra_data: ExtraFieldsV2 { round: p.round + 1, quorum_cert: p.qc.clone() }")],
        "xone-builder",
        "producer_construction",
    ),
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
#[ignore = "builds the crates once per mutant"]
fn the_producer_tests_catch_every_mutant() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = std::env::var("PRODUCER_MUTANT_TARGET")
        .map_or_else(|_| crates.join("../target/producer-mutants"), PathBuf::from);
    let filter = std::env::var("MUTANT_FILTER").unwrap_or_default();
    let mut survivors = vec![];
    for (name, edits, package, test) in MUTANTS.iter().filter(|m| m.0.contains(&filter)) {
        let mut saved = vec![];
        let mut files: Vec<(PathBuf, String)> = vec![];
        for (file, from, to) in *edits {
            let path = crates.join(file);
            let current = match files.iter().find(|(p, _)| *p == path) {
                Some((_, t)) => t.clone(),
                None => {
                    let original = fs::read_to_string(&path).unwrap();
                    saved.push((path.clone(), original.clone()));
                    original
                }
            };
            assert_eq!(current.matches(from).count(), 1, "`{from}` must occur once in {file}");
            files.retain(|(p, _)| *p != path);
            files.push((path, current.replace(from, to)));
        }
        let guard = Restore(saved);
        for (p, t) in &files {
            fs::write(p, t).unwrap();
        }
        let out = Command::new(env!("CARGO"))
            .args(["test", "--offline", "--locked", "-p", package, "--test", test])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(&crates)
            .output()
            .unwrap();
        drop(guard);
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        let compiled = !text.contains("could not compile");
        let caught = compiled && !out.status.success() && (text.contains("test result: FAILED") || text.contains("panicked"));
        let by: Vec<&str> = text.lines().filter(|l| l.starts_with("test ") && l.ends_with("FAILED")).collect();
        println!(
            "mutant {name} [{package} {test}]: {} {by:?}",
            if caught { "CAUGHT" } else if !compiled { "DID NOT COMPILE" } else { "SURVIVED" }
        );
        if !caught {
            survivors.push(*name);
        }
    }
    assert!(survivors.is_empty(), "mutants not caught: {survivors:?}");
}
