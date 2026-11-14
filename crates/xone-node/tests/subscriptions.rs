#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! WebSocket subscriptions (issue #122): a node that imports blocks publishes one `newHeads`
//! notification per canonical block and the logs of each block to the `logs` subscriptions whose
//! filter matches, and a reorg sends the logs of the removed blocks with `removed: true` before the
//! new branch (geth `eth/filters/filter_system.go:453`, `core/blockchain.go:3572-3653`).

mod common;

use std::{net::TcpStream, time::Duration};

use common::{call, free_port, parts, scratch, vectors};
use serde_json::{json, Value};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
use xone_node::{
    datadir,
    services::{self, core::now},
    Config,
};

const STAMP: &str = "xOne/v0.0.0-test/linux-x86_64/rustc1";
/// The BlockSigners contract: each sign transaction leaves one log of it (geth `core/state_processor.go:900-911`).
const SIGNERS: &str = "0x0000000000000000000000000000000000000089";

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn next(ws: &mut Ws) -> Value {
    loop {
        match ws.read().expect("a message within the timeout") {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            m => panic!("{m:?}"),
        }
    }
}

fn subscribe(ws: &mut Ws, id: u64, params: Value) -> String {
    let req = json!({"jsonrpc": "2.0", "id": id, "method": "eth_subscribe", "params": params});
    ws.send(Message::text(req.to_string())).unwrap();
    let r = next(ws);
    assert_eq!(r["id"], json!(id), "{r}");
    r["result"].as_str().expect("subscription id").to_owned()
}

/// The next `n` notifications, as (subscription, result).
fn notes(ws: &mut Ws, n: usize) -> Vec<(String, Value)> {
    (0..n)
        .map(|_| {
            let m = next(ws);
            assert_eq!(m["method"], "eth_subscription", "{m}");
            let p = &m["params"];
            (
                p["subscription"].as_str().unwrap().to_owned(),
                p["result"].clone(),
            )
        })
        .collect()
}

#[test]
fn new_heads_and_logs_are_published_for_each_imported_block_and_on_a_reorg() {
    let blocks = vectors(17);
    let dir = scratch("subs");
    let (http, wsp) = (free_port(), free_port());
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.http.enabled = true;
    cfg.http.port = http;
    cfg.http.api = vec!["eth".into()];
    cfg.ws.enabled = true;
    cfg.ws.port = wsp;
    cfg.ws.api = vec!["eth".into()];
    cfg.ipc.disabled = true;
    let (marker, spec, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, spec, &genesis, STAMP).unwrap();
    built.node.start().unwrap();

    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{wsp}")).unwrap();
    if let MaybeTlsStream::Plain(s) = ws.get_mut() {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    let heads = subscribe(&mut ws, 1, json!(["newHeads"]));
    let signers = subscribe(&mut ws, 2, json!(["logs", {"address": SIGNERS}]));
    let other = subscribe(
        &mut ws,
        3,
        json!(["logs", {"address": "0x00000000000000000000000000000000000000aa"}]),
    );

    for b in &blocks[1..=17] {
        let (h, body) = parts(b);
        built.core.import(h, body, now()).unwrap();
    }
    let want_logs = call(
        http,
        "eth_getLogs",
        json!([{"fromBlock": "0x1", "toBlock": "0x11", "address": SIGNERS}]),
    );
    let want_logs = want_logs.as_array().unwrap().clone();
    assert!(
        !want_logs.is_empty(),
        "blocks 16 and 17 hold sign transactions"
    );
    let got = notes(&mut ws, 17 + want_logs.len());
    assert!(
        got.iter().all(|(s, _)| *s != other),
        "a filter that matches nothing gets nothing"
    );

    // one newHeads per block, in order, equal to eth_getBlockByNumber without the body fields
    let got_heads: Vec<_> = got.iter().filter(|(s, _)| *s == heads).collect();
    assert_eq!(got_heads.len(), 17);
    for (n, (_, h)) in (1u64..).zip(&got_heads) {
        let mut b = call(
            http,
            "eth_getBlockByNumber",
            json!([format!("{n:#x}"), false]),
        );
        let o = b.as_object_mut().unwrap();
        for k in ["transactions", "uncles", "size", "totalDifficulty"] {
            o.remove(k);
        }
        assert_eq!(*h, b, "block {n}");
    }
    let got_logs: Vec<Value> = got
        .iter()
        .filter(|(s, _)| *s == signers)
        .map(|(_, l)| l.clone())
        .collect();
    assert_eq!(got_logs, want_logs);

    // a reorg back to 15 and the same two blocks again: the removed logs, then heads 16, 17 and the logs
    built.core.unwind_to(15).unwrap();
    for b in &blocks[16..=17] {
        let (h, body) = parts(b);
        built.core.import(h, body, now()).unwrap();
    }
    let removed: Vec<Value> = want_logs
        .iter()
        .map(|l| {
            let mut l = l.clone();
            l["removed"] = json!(true);
            l
        })
        .collect();
    let got = notes(&mut ws, 2 + 2 * want_logs.len());
    let relogs: Vec<Value> = got
        .iter()
        .filter(|(s, _)| *s == signers)
        .map(|(_, l)| l.clone())
        .collect();
    assert_eq!(relogs[..removed.len()], removed[..]);
    assert_eq!(relogs[removed.len()..], want_logs[..]);
    let reheads: Vec<_> = got
        .iter()
        .filter(|(s, _)| *s == heads)
        .map(|(_, h)| h["number"].clone())
        .collect();
    assert_eq!(reheads, vec![json!("0x10"), json!("0x11")]);

    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
