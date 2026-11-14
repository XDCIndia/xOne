#![allow(clippy::unwrap_used, clippy::expect_used)] // test: a panic is the failure report
//! #188: `docs/rpc/methods.md` is generated from the registries of `policy::registry`, so it
//! covers every served method and fails when one is added or removed
//! (`UPDATE_DOCS=1 cargo test -p xone-rpc --test rpc_doc` rewrites it).

use xone_rpc::{
    policy,
    server::{Cost, Limits},
};

/// The `--http.api` / `--ws.api` names, in the order of the document.
const NAMESPACES: &[(&str, &str)] = &[
    (
        "eth",
        "Ethereum methods, the XDC extensions served under `eth_`, and the state-heal status",
    ),
    (
        "xdc",
        "the `xdc_` prefix geth-XDC serves for the eth block-chain methods",
    ),
    ("net", "network identity and peer count"),
    ("web3", "client version"),
    ("txpool", "the transaction pool"),
    (
        "admin",
        "node and peer administration (never enabled by default)",
    ),
    ("debug", "raw data and tracers (never enabled by default)"),
    ("xdpos", "XDPoS consensus state (`XDPoS_` methods)"),
];

fn class(c: Cost, l: &Limits) -> String {
    let i = match c {
        Cost::C0 => 0,
        Cost::C1 => 1,
        Cost::C2 => 2,
        Cost::C3 => 3,
    };
    format!("{c:?} ({} units, {} s)", c.units(), l.deadline[i].as_secs())
}

fn render() -> String {
    let l = Limits::default();
    let mut s = String::from(
        "# JSON-RPC methods\n\n\
Generated from the method registries (`crates/xone-rpc/src/policy.rs`) by \
`crates/xone-rpc/tests/rpc_doc.rs`; do not edit by hand \
(`UPDATE_DOCS=1 cargo test -p xone-rpc --test rpc_doc` rewrites it). Scope and parity notes: \
`spec/rpc/scope-adr-008.md`.\n\n\
A listener serves only the namespaces of its `--http.api`, `--ws.api` (or IPC) list. `eth_subscribe` and `eth_unsubscribe` (`newHeads`, `logs`, \
`newPendingTransactions`) are served on WebSocket and IPC only.\n\n\
Class is the ADR-007 cost class: the token-bucket units the call takes and its deadline.\n\n\
Errors every method can return:\n\n\
| Code | Meaning |\n|---|---|\n\
| -32700 | parse error |\n\
| -32600 | invalid request (envelope, batch or body too large) |\n\
| -32601 | method not found, or its namespace is not enabled on this listener |\n\
| -32602 | invalid params |\n\
| -32603 | internal error |\n\
| -32005 | limit exceeded (rate bucket, class concurrency, node busy); `data` is the retry delay |\n\
| -32000 | server error, including the typed unsupported-history error for data outside the retained window |\n",
    );
    for (ns, what) in NAMESPACES {
        let reg = policy::registry(&[(*ns).to_owned()]).unwrap();
        let names = reg.names();
        s += &format!("\n## `{ns}`: {what}\n\n| Method | Class |\n|---|---|\n");
        for n in names {
            let (c, _) = reg.get(n).unwrap();
            s += &format!("| `{n}` | {} |\n", class(*c, &l));
        }
    }
    s
}

#[test]
#[allow(clippy::disallowed_methods)] // test-only doc bless switch, never in a shipped path
fn rpc_reference_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/rpc/methods.md");
    let want = render();
    if std::env::var_os("UPDATE_DOCS").is_some() {
        std::fs::write(path, &want).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(path).unwrap_or_else(|_| String::new()),
        want,
        "run UPDATE_DOCS=1 cargo test -p xone-rpc --test rpc_doc"
    );
}

#[test]
fn every_registered_method_is_in_a_documented_namespace() {
    let all: Vec<String> = NAMESPACES.iter().map(|(n, _)| (*n).to_owned()).collect();
    let full = policy::registry(&all).unwrap();
    let doc = render();
    for n in full.names() {
        assert!(doc.contains(&format!("`{n}`")), "{n} is not documented");
    }
}
