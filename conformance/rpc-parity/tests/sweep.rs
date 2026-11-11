#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! The sweep is a gate: no recorded geth-XDC answer that xone-rpc can be replayed on may differ.

use serde_json::Value;
use std::process::Command;

fn sweep(net: &str) -> Value {
    let out = std::env::temp_dir().join(format!("rpc-parity-{net}-{}", std::process::id()));
    let st = Command::new(env!("CARGO_BIN_EXE_rpc-parity"))
        .args(["--network", net, "--out", out.to_str().unwrap()])
        .status()
        .unwrap();
    let rep: Value =
        serde_json::from_slice(&std::fs::read(out.join(format!("{net}.json"))).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&out);
    assert!(
        st.success(),
        "the sweep found a difference: {}",
        rep["summary"]
    );
    rep
}

#[test]
fn apothem_replay_window_has_no_difference_and_compares_real_rows() {
    let r = sweep("apothem");
    assert_eq!(r["summary"]["differ"], 0);
    assert!(
        r["summary"]["equal"].as_u64().unwrap() > 400,
        "{}",
        r["summary"]
    );
    // Every method of the issue's list has at least one compared row or a stated reason.
    let rows = r["rows"].as_array().unwrap();
    assert!(rows
        .iter()
        .all(|x| x["status"] != "not-backed" || !x["note"].as_str().unwrap().is_empty()));
}

#[test]
fn net2040_is_reported_not_backed_never_skipped() {
    let r = sweep("net2040");
    assert_eq!(r["summary"]["differ"], 0);
    assert!(r["summary"]["not_backed"].as_u64().unwrap() > 1000);
}
