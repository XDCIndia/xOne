#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! Mutation check of the corpus: disable one check at a time in the verifier source,
//! run the corpus tests in a scratch target directory and require them to fail, then
//! restore the file. Slow (one build per mutant), so it only runs on request:
//!
//!     cargo test -p xone-consensus --test mutants -- --ignored --nocapture

use std::{fs, path::PathBuf, process::Command};

/// (file under src, text that occurs once, replacement that disables the check). The test targets
/// that must fail are `targets_of(file)`.
const MUTANTS: &[(&str, &str, &str)] = &[
    ("verify/header.rs", "if round <= qc_round {", "if false {"),
    ("verify/header.rs", "if info.round > 0 && (signers.len() as f64) < need {", "if false {"),
    ("verify/header.rs", "if EcdsaSignature::from_slice(&sig[..64])", "if false && EcdsaSignature::from_slice(&sig[..64])"),
    // Not the byte-level duplicate skip: that one is an optimisation of geth (same address, counted once),
    // so removing it changes nothing. This counts raw signatures instead of distinct signers.
    ("verify/header.rs", "if info.round > 0 && (signers.len() as f64) < need {", "if info.round > 0 && (qc.signatures.len() as f64) < need {"),
    ("verify/header.rs", "if !masternodes.contains(&who) {", "if false {"),
    ("verify/header.rs", "if expected != qc.gap_number {", "if false {"),
    ("verify/header.rs", "if !same_set(&ctx.masternodes, &listed) {", "if false {"),
    ("verify/header.rs", "if self.spec.validator_order.is_active(h.number) && ctx.masternodes != listed {", "if false {"),
    ("verify/header.rs", "if !same_set(&ctx.penalties, &addresses(&h.penalties)) {", "if false {"),
    ("verify/header.rs", "} else if !h.validators.is_empty() || !h.penalties.is_empty() {", "} else if false {"),
    ("verify/header.rs", "if ctx.masternodes[leader] != validator {", "if false {"),
    ("verify/header.rs", "if !ctx.masternodes.contains(&validator) {", "if false {"),
    ("verify/header.rs", "Some(have) if have != want => {", "Some(have) if false && have != want => {"),
    ("verify/header.rs", "if !same_set(&listed, &ctx.checkpoint_signers) {", "if false {"),
    ("verify/header.rs", "let (n, epoch) = (h.number, x.epoch);\n        if h.timestamp > now {", "let (n, epoch) = (h.number, x.epoch);\n        if false {"),
    ("verify/header.rs", "n => return Err(HeaderError::ValidatorSignatureLength(n)),\n        }\n        if h.timestamp > now {", "n => return Err(HeaderError::ValidatorSignatureLength(n)),\n        }\n        if false {"),
    ("verify/header.rs", "if checkpoint && !h.coinbase.is_zero() {", "if false {"),
    ("verify/header.rs", "if h.difficulty != U256::from(1) {", "if false {"),
    ("verify/header.rs", "if h.gas_used > h.gas_limit {", "if false {"),
    ("verify/header.rs", "if h.validators.is_empty() {\n                return Err(HeaderError::EmptyEpochSwitchValidators);", "if false {\n                return Err(HeaderError::EmptyEpochSwitchValidators);"),
    // ---- crate::v1: each rule of the module, caught by v1_negative or v1_vectors
    ("v1/checkpoint.rs", "if !same_set(listed, expected) {", "if false {"),
    ("v1/checkpoint.rs", "if listed != expected {", "if false {"),
    ("v1/checkpoint.rs", "sort_slice(&mut sorted, |a, b| a.1 >= b.1);", "sort_slice(&mut sorted, |a, b| a.1 > b.1);"),
    ("v1/checkpoint.rs", "if header_penalties != bytes {", "if false {"),
    ("v1/checkpoint.rs", "set.remove(p);", "let _ = p;"),
    ("v1/penalty.rs", "< MINIMUM_MINER_BLOCK_PER_EPOCH)", "<= MINIMUM_MINER_BLOCK_PER_EPOCH)"),
    ("v1/turn.rs", "if have != want {", "if false {"),
    ("v1/turn.rs", "if !signers.contains(&creator) && !masternodes.contains(&creator) {", "if false {"),
    ("v1/turn.rs", "if masternodes.len() > 1 && !checkpoint && recent_creator == Some(creator) {", "if false {"),
    ("v1/double_validation.rs", "if validator != assigned()? {", "if false {"),
    ("v1/double_validation.rs", "if h.validator.is_empty() {", "if false {"),
    ("v1/double_validation.rs", "let move_m2 = if tip_randomize {", "let move_m2 = if false {"),
    ("v1/switch.rs", "Some(0) => Ok(()),\n        Some(_) => Err(", "Some(_) => Ok(()),\n        None => Err("),
    ("v1/snapshot.rs", "snap.recents.insert(number, signer);", "let _ = signer;"),
    ("v1/snapshot.rs", "snap.votes.clear();", "let _ = &snap.votes;"),
    ("v1/mod.rs", "if parent_time.wrapping_add(period) > time {", "if parent_time.wrapping_add(2) > time {"),
    ("verify/header.rs", "crate::v1::check_timestamp(parent.timestamp, h.timestamp, x.period)?;", "crate::v1::check_timestamp(parent.timestamp, h.timestamp, 2)?;"),
    ("verify/body.rs", "if header.tx_root != computed {", "if false {"),
    ("verify/body.rs", "if header.receipt_root != computed {", "if false {"),
    ("verify/body.rs", "if bloom != header.bloom {", "if false {"),
    ("verify/body.rs", "if header.uncle_hash != computed {", "if false {"),
];

/// The tests that must catch a mutant of `file`: the V1 rules by their own vectors, the rest by the corpus.
fn targets_of(file: &str) -> &'static [&'static str] {
    match file {
        f if f.starts_with("v1/") => &["v1_negative", "v1_vectors"],
        "verify/header.rs" => &["corpus", "v1_negative"],
        _ => &["corpus"],
    }
}

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
fn the_corpus_catches_every_mutant() {
    let target = crate_dir().join("../../target/mutants");
    let mut survivors = vec![];
    for (i, (file, from, to)) in MUTANTS.iter().enumerate() {
        let path = crate_dir().join("src").join(file);
        let original = fs::read_to_string(&path).unwrap();
        assert_eq!(
            original.matches(from).count(),
            1,
            "mutant {i}: `{from}` must occur once in {file}"
        );
        let guard = Restore(path.clone(), original.clone());
        fs::write(&path, original.replace(from, to)).unwrap();
        let out = Command::new(env!("CARGO"))
            .args(["test", "--offline", "--locked", "-p", "xone-consensus"])
            .args(targets_of(file).iter().flat_map(|t| ["--test", t]))
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
        let caught = !out.status.success() && text.contains("test result: FAILED");
        let by: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("test ") && l.ends_with("FAILED"))
            .collect();
        println!(
            "mutant {i:2} {file}: `{}` -> {} {by:?}",
            from.lines().next().unwrap(),
            if caught { "CAUGHT" } else { "SURVIVED" }
        );
        if !caught {
            survivors.push(format!("{file}: {from}"));
        }
    }
    assert!(
        survivors.is_empty(),
        "mutants the corpus did not catch:\n{}",
        survivors.join("\n")
    );
}
