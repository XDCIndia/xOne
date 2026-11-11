#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
#![allow(clippy::disallowed_methods)] // the ignored live test finds its binaries through the environment
//! The first follow against a private geth-XDC node on loopback (docs/ops/first-follow.md): connect,
//! import to the geth head, follow 50 more blocks with a head lag of at most 2, compare RPC answers,
//! stop on SIGTERM within 5 s and resume at the same head. Ignored: it needs a geth-XDC binary built
//! from the pin and minutes of mining.
//!
//! ```text
//! XONE_GETH_BIN=/path/geth-xdc XONE_HOLDER=/path/to/local_geth-test-binary WORK=/tmp/xl \
//!   cargo test --locked -p xone-node --test live -- --ignored --nocapture
//! ```
//! (`WORK` must be a short path: it holds geth's unix socket.)

use std::process::Command;

#[test]
#[ignore = "needs geth-XDC built from the pin; see the module docs"]
fn follows_a_local_geth_and_resumes() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/live/run.sh");
    let xone = std::env::var("XONE_BIN").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/xone").to_owned()
    });
    let status = Command::new("bash")
        .arg(script)
        .env("XONE_BIN", xone)
        .status()
        .unwrap();
    assert!(status.success(), "{status}");
}
