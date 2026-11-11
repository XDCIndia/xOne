//! The sweep's special mode: single transactions to 0x89, 0x90, 0x91 to 0x94 and the out-of-gas call
//! to 0x88, replayed through the router (`conformance/vectors/special/cases/`, exported by
//! `xone-sweep special cases` and `oog`). Each class must replay with no divergence, and, where a rule
//! decides the class, diverge under the wrong rule.

use std::path::PathBuf;
use xone_evm::spec::XdcRules;
use xone_sweep::{compare, replay, replay_with, Case};

fn cases(class: &str) -> Vec<Case> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/special/cases");
    let mut v: Vec<Case> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{class}-"))
        })
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    v.sort_by_key(|c| c.block.number);
    v
}

fn clean(c: &Case, tweak: &dyn Fn(&mut XdcRules)) -> bool {
    replay_with(c, tweak).is_ok_and(|o| compare(c, &o).is_none())
}

/// Replays every case of `class` clean and every one diverges under `mutant`.
fn class(name: &str, want: usize, mutant: Option<&dyn Fn(&mut XdcRules)>) {
    let cs = cases(name);
    assert!(cs.len() >= want, "{name}: {} cases, want {want}", cs.len());
    for c in &cs {
        let o = replay(c).unwrap_or_else(|e| panic!("{}: {e}", c.name));
        if let Some(d) = compare(c, &o) {
            panic!("{} ({}): {d}", c.name, c.tx.hash);
        }
        if let Some(m) = mutant {
            assert!(!clean(c, m), "{}: its mutant replays clean", c.name);
        }
    }
    eprintln!("{name}: {} cases, no divergence", cs.len());
}

#[test]
fn sign_txs_before_tip_signing_run_in_the_evm() {
    class("sign-evm", 30, Some(&|r| r.sign_tx_route = true));
}

/// Named mutant: sign transactions are charged gas (they go to the EVM) after `TIPSigning`.
#[test]
fn sign_txs_after_tip_signing_use_no_evm_and_no_gas() {
    for c in ["sign-v1-before-trc21", "sign-v1-after-trc21", "sign-v2"] {
        class(c, 5, Some(&|r| r.sign_tx_route = false));
    }
}

#[test]
fn randomize_txs_run_in_the_evm_and_pay_no_fee() {
    class("randomize", 3, None);
}

#[test]
fn xdcx_empty_txs_use_no_evm_no_gas_and_no_nonce_bump() {
    class("xdcx-empty", 1, Some(&|r| r.xdcx_empty_route = false));
}

/// An out-of-gas call to 0x88 charges all of its gas, as the archive's geth does.
#[test]
fn out_of_gas_call_to_0x88_charges_all_gas() {
    let cs = cases("oog-0x88");
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].receipt.gas_used, cs[0].tx.gas);
    class("oog-0x88", 1, None);
}
