#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! #60 through the packaged entry point (`services::build`, the code `xone` runs): `--rpc.jwtsecret`
//! closes HTTP and the WebSocket upgrade before dispatch, a bad secret file stops the start without
//! echoing the file, and the IPC socket is the owner's only. The token checks themselves are in
//! `xone-rpc/tests/rpc.rs`.

mod common;

use std::{
    io::{Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
};

use common::{free_port, scratch};
use xone_node::{datadir, services, Config};

const STAMP: &str = "xOne/v0.0.0-test/linux-x86_64/rustc1";

fn cfg(dir: &std::path::Path, http: u16, ws: u16) -> Config {
    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.datadir = dir.to_owned();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.http.enabled = true;
    cfg.http.port = http;
    cfg.ws.enabled = true;
    cfg.ws.port = ws;
    cfg
}

fn post(port: u16, extra: &str) -> String {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}"#;
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: localhost\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn the_node_entry_point_enforces_the_jwt_secret_and_the_ipc_mode() {
    let dir = scratch("rpcsec");
    std::fs::create_dir_all(&dir).unwrap();
    let secret = dir.join("jwt.hex");
    std::fs::write(&secret, format!("0x{}\n", "ab".repeat(32))).unwrap();
    let (http, ws) = (free_port(), free_port());
    let mut c = cfg(&dir, http, ws);
    c.set("rpc.jwtsecret", secret.to_str().unwrap()).unwrap();
    let (marker, spec, genesis) = datadir::prepare_chain(&c).unwrap();
    let mut built = services::build(&c, &marker, spec, &genesis, STAMP).unwrap();
    built.node.start().unwrap();

    for extra in [
        "",
        "Authorization: Bearer a.b.c\r\n",
        "X-Forwarded-For: 127.0.0.1\r\n",
    ] {
        let out = post(http, extra);
        assert!(out.starts_with("HTTP/1.1 401"), "{extra:?}: {out}");
        assert!(!out.contains("result"), "{out}");
    }
    match tungstenite::connect(format!("ws://127.0.0.1:{ws}")) {
        Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        other => panic!("upgrade not refused: {:?}", other.map(|_| ())),
    }
    let ipc = dir.join("xone.ipc");
    for _ in 0..50 {
        if ipc.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mode = std::fs::metadata(&ipc).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    built.node.stop().unwrap();
    drop(built);

    // a secret that is not 32 bytes of hex, or no file, stops the start; the content is not echoed
    for (name, content) in [("short.hex", Some("0xdeadbeefcafe")), ("absent.hex", None)] {
        let p = dir.join(name);
        if let Some(t) = content {
            std::fs::write(&p, t).unwrap();
        }
        let mut c = cfg(&dir, free_port(), free_port());
        c.set("rpc.jwtsecret", p.to_str().unwrap()).unwrap();
        let (marker, spec, genesis) = datadir::prepare_chain(&c).unwrap();
        let e = services::build(&c, &marker, spec, &genesis, STAMP)
            .err()
            .expect("refused");
        let e = e.to_string();
        assert!(e.contains("--rpc.jwtsecret"), "{e}");
        assert!(!e.contains("deadbeef"), "{e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
