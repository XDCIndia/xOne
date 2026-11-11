#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! Every switch reads the chain spec: no source file of this crate compares a chain id to a literal.
//! The chain-id rules live in `xone-chainspec` (`is_pre_eip2028_calldata`, `installs_multicall3`).

use std::path::Path;

/// Offending lines of `text`: `== 50`, `== 51`, the literal 5551 and `chain_id ==`.
fn offences(text: &str) -> Vec<String> {
    let literal = |line: &str, pat: &str| {
        line.match_indices(pat).any(|(i, _)| {
            let before = line[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_digit());
            let after = line[i + pat.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
            !before && !after
        })
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            literal(l, "== 50")
                || literal(l, "== 51")
                || literal(l, "5551")
                || l.contains("chain_id ==")
        })
        .map(|(n, l)| format!("line {}: {}", n + 1, l.trim()))
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            for o in offences(&std::fs::read_to_string(&p).unwrap()) {
                out.push(format!("{}: {o}", p.display()));
            }
        }
    }
}

#[test]
fn fixture_lines_are_detected() {
    for bad in [
        "if chain_id == 5551 {",
        "if cfg.chain_id == 50 {",
        "x == 50",
        "x == 51 ||",
        "let devnet = 5551;",
        "// chain 5551 keeps 68",
        "assert!(self.chain_id == other)",
    ] {
        assert_eq!(offences(bad).len(), 1, "{bad}");
    }
    for ok in [
        "x == 500",
        "x == 5100",
        "let n = 55512;",
        "chain_id: spec.chain_id,",
        "cfg.chain_id = r.chain_id;",
    ] {
        assert!(offences(ok).is_empty(), "{ok}");
    }
}

#[test]
fn no_literal_chain_id_in_xone_evm_sources() {
    let mut found = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert!(
        found.is_empty(),
        "literal chain id in xone-evm:\n{}",
        found.join("\n")
    );
}
