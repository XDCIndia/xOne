//! Shared by the integration tests: the first blocks of the committed Apothem replay vectors, and the
//! conversions between the wire block, the stored body and a raw HTTP JSON-RPC call.
#![allow(dead_code)]

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
};

use alloy_rlp::Decodable;
use flate2::read::GzDecoder;
use serde_json::{json, Value};
use xone_consensus::verify::Body;
use xone_net::eth::Block as WireBlock;
use xone_node::services::from_wire;
use xone_primitives::Header;

/// `conformance/vectors/replay/apothem-0-1800.jsonl.gz`: blocks `0..=count`, one wire block each.
pub fn vectors(count: usize) -> Vec<WireBlock> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/replay/apothem-0-1800.jsonl.gz");
    let mut text = String::new();
    GzDecoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{e}")))
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("{e}"));
    text.lines()
        .take(count + 1)
        .enumerate()
        .map(|(i, l)| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(v["n"], json!(i));
            let raw = alloy_primitives::hex::decode(v["raw"].as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
            WireBlock::decode(&mut &raw[..]).unwrap_or_else(|e| panic!("block {i}: {e}"))
        })
        .collect()
}

pub fn parts(b: &WireBlock) -> (Header, Body) {
    (
        b.header.clone(),
        Body {
            transactions: b.transactions.iter().map(from_wire).collect(),
            uncles: b.uncles.clone(),
            withdrawals: None,
        },
    )
}

pub fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xone-node-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or_else(|e| panic!("{e}"))
}

/// One JSON-RPC call over HTTP; the `result`.
pub fn call(port: u16, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| panic!("{e}"));
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap_or_else(|e| panic!("{e}"));
    let json = out.split("\r\n\r\n").nth(1).unwrap_or_default();
    let v: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert!(v.get("error").is_none(), "{method}: {v}");
    v["result"].clone()
}
