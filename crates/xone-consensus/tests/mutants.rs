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

/// (file under src/verify, text that occurs once, replacement that disables the check, what the corpus is expected to say)
const MUTANTS: &[(&str, &str, &str)] = &[
    ("header.rs", "if round <= qc_round {", "if false {"),
    ("header.rs", "if info.round > 0 && (signers.len() as f64) < need {", "if false {"),
    ("header.rs", "if EcdsaSignature::from_slice(&sig[..64])", "if false && EcdsaSignature::from_slice(&sig[..64])"),
    // Not the byte-level duplicate skip: that one is an optimisation of geth (same address, counted once),
    // so removing it changes nothing. This counts raw signatures instead of distinct signers.
    ("header.rs", "if info.round > 0 && (signers.len() as f64) < need {", "if info.round > 0 && (qc.signatures.len() as f64) < need {"),
    ("header.rs", "if !masternodes.contains(&who) {", "if false {"),
    ("header.rs", "if expected != qc.gap_number {", "if false {"),
    ("header.rs", "if !same_set(&ctx.masternodes, &listed) {", "if false {"),
    ("header.rs", "if self.spec.validator_order.is_active(h.number) && ctx.masternodes != listed {", "if false {"),
    ("header.rs", "if !same_set(&ctx.penalties, &addresses(&h.penalties)) {", "if false {"),
    ("header.rs", "} else if !h.validators.is_empty() || !h.penalties.is_empty() {", "} else if false {"),
    ("header.rs", "if ctx.masternodes[leader] != validator {", "if false {"),
    ("header.rs", "if !ctx.masternodes.contains(&validator) {", "if false {"),
    ("header.rs", "Some(have) if have != want => {", "Some(have) if false && have != want => {"),
    ("header.rs", "if validator != assigned {", "if false {"),
    ("header.rs", "if !same_set(&listed, &ctx.checkpoint_signers) {", "if false {"),
    ("header.rs", "let (n, epoch) = (h.number, x.epoch);\n        if h.timestamp > now {", "let (n, epoch) = (h.number, x.epoch);\n        if false {"),
    ("header.rs", "n => return Err(HeaderError::ValidatorSignatureLength(n)),\n        }\n        if h.timestamp > now {", "n => return Err(HeaderError::ValidatorSignatureLength(n)),\n        }\n        if false {"),
    ("header.rs", "if want != 0 && have != want {", "if false {"),
    ("header.rs", "if !ctx.signers.contains(&creator) && !ctx.masternodes.contains(&creator) {", "if false {"),
    ("header.rs", "if checkpoint && !h.coinbase.is_zero() {", "if false {"),
    ("header.rs", "if h.difficulty != U256::from(1) {", "if false {"),
    ("header.rs", "if h.gas_used > h.gas_limit {", "if false {"),
    ("header.rs", "if h.validators.is_empty() {\n                return Err(HeaderError::EmptyEpochSwitchValidators);", "if false {\n                return Err(HeaderError::EmptyEpochSwitchValidators);"),
    ("body.rs", "if header.tx_root != computed {", "if false {"),
    ("body.rs", "if header.receipt_root != computed {", "if false {"),
    ("body.rs", "if bloom != header.bloom {", "if false {"),
    ("body.rs", "if header.uncle_hash != computed {", "if false {"),
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
fn the_corpus_catches_every_mutant() {
    let target = crate_dir().join("../../target/mutants");
    let mut survivors = vec![];
    for (i, (file, from, to)) in MUTANTS.iter().enumerate() {
        let path = crate_dir().join("src/verify").join(file);
        let original = fs::read_to_string(&path).unwrap();
        assert_eq!(
            original.matches(from).count(),
            1,
            "mutant {i}: `{from}` must occur once in {file}"
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
                "corpus",
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
