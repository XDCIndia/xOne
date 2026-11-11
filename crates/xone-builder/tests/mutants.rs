#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the sources are trusted inputs and a panic is the failure signal"
)]
//! Mutation check of the pool rules: flip one rule at a time in the source, run the pool tests in a scratch
//! target directory, require them to fail, restore the file. One build per mutant, so it runs on request:
//!
//!     cargo test -p xone-builder --test mutants -- --ignored --nocapture

use std::{fs, path::PathBuf, process::Command};

/// (name, file under src/txpool, text that occurs once, replacement, test target that must fail).
const MUTANTS: &[(&str, &str, &str, &str, &str)] = &[
    ("non-candidate sign tx admitted", "validate.rs", "if tx.is_special() && !pool.authority.is_signer(&t.from) {", "if false {", "rules"),
    ("tier floor applied to a special tx", "validate.rs", "if !tx.is_special() && tx.fee_cap() < min_gas_price(&pool.spec, pool.head.number) {", "if tx.fee_cap() < min_gas_price(&pool.spec, pool.head.number) {", "rules"),
    ("pool minimum tip applied to a special tx", "validate.rs", "if !tx.is_special() && tx.tip_cap() < pool.min_tip {", "if tx.tip_cap() < pool.min_tip {", "rules"),
    ("tier floor off by one", "validate.rs", "tx.fee_cap() < min_gas_price(&pool.spec, pool.head.number) {\n        return Err(Error::UnderMinGasPrice);", "tx.fee_cap() <= min_gas_price(&pool.spec, pool.head.number) {\n        return Err(Error::UnderMinGasPrice);", "rules"),
    ("balance read as the whole wide value", "validate.rs", "acct.as_ref().map_or(U256::ZERO, |a| a.balance.low());", "acct.as_ref().map_or(U256::ZERO, |a| a.balance.to_u256().unwrap_or(U256::MAX));", "rules"),
    ("price bump off", "list.rs", "let pct = U256::from(100u64.saturating_add(bump));", "let pct = U256::from(100u64);", "rules"),
    ("regular tx replaces a special one", "list.rs", "if o.is_special() && !tx.is_special() {\n                return Add::Rejected;", "if false {\n                return Add::Rejected;", "oracle_scenarios"),
    ("per-account queue bound off", "queued.rs", "dropped.extend(l.cap(account_queue).iter().map(|t| t.hash));", "dropped.extend(l.cap(usize::MAX).iter().map(|t| t.hash));", "rules"),
    ("per-account executable bound off", "pending.rs", "if l.len() > acct_slots {", "if false {", "oracle_scenarios"),
    ("promotion of a nonce-gapped tx", "list.rs", "if next > start {\n            return out;\n        }", "if false {\n            return out;\n        }", "rules"),
    ("queue expiry at the lifetime instead of past it", "queued.rs", "if idle > lifetime {", "if idle >= lifetime {", "rules"),
    ("sign calldata of any length", "special.rs", "d.len() == SIGN_CALLDATA_LEN && sel == SIGN_SELECTOR", "sel == SIGN_SELECTOR", "oracle_scenarios"),
    ("signing slot expires at its last block", "special.rs", "head > expiry", "head >= expiry", "oracle_scenarios"),
];

struct Restore(PathBuf, String);
impl Drop for Restore {
    fn drop(&mut self) {
        fs::write(&self.0, &self.1).unwrap();
    }
}

#[test]
#[ignore = "builds the crate once per mutant"]
fn the_pool_tests_catch_every_mutant() {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/txpool-mutants");
    let mut survivors = vec![];
    for (name, file, from, to, test) in MUTANTS {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/txpool")
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
                "xone-builder",
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
