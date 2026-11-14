#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Metrics push (issue #186): the pusher PUTs the `/metrics` page to a Pushgateway-shaped local
//! listener, and a dead gateway is counted, not waited on.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{atomic::Ordering::SeqCst, Arc},
    time::{Duration, Instant},
};

use xone_node::{
    lifecycle::Service,
    observe::{
        metrics::{MetricsSource, Snapshot},
        push::{target, PushCfg, Pusher},
    },
};

struct Src;
impl MetricsSource for Src {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: "test".into(),
            head_number: Some(7),
            ..Snapshot::default()
        }
    }
}

#[test]
fn url_rules() {
    let t = target("http://gw:9091", "xone").unwrap();
    assert_eq!(
        (t.host.as_str(), t.path.as_str()),
        ("gw:9091", "/metrics/job/xone")
    );
    let t = target("http://gw/pfx/", "n-1").unwrap();
    assert_eq!(
        (t.host.as_str(), t.path.as_str()),
        ("gw:80", "/pfx/metrics/job/n-1")
    );
    assert!(target("https://gw", "x").is_err());
    assert!(target("http://u:p@gw:9091", "x").is_err());
    assert!(target("http://gw:9091", "a/b").is_err());
}

#[test]
fn pushes_the_metrics_page() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let mut p = Pusher::new(
        &PushCfg {
            url,
            job: "node-1".into(),
            interval: Duration::from_secs(1),
        },
        Arc::new(Src),
    )
    .unwrap();
    p.start().unwrap();
    let (mut s, _) = l.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&got).contains("xone_metrics_push_failures_total 0") {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "closed early");
        got.extend_from_slice(&buf[..n]);
    }
    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let text = String::from_utf8_lossy(&got);
    assert!(
        text.starts_with("PUT /metrics/job/node-1 HTTP/1.1\r\n"),
        "{text}"
    );
    assert!(text.contains("xone_head_block_number"), "{text}");
    p.stop();
    assert_eq!(p.failures().load(SeqCst), 0);
}

#[test]
fn dead_gateway_is_counted_and_stop_is_prompt() {
    // a port with nothing listening
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut p = Pusher::new(
        &PushCfg {
            url: format!("http://127.0.0.1:{port}"),
            job: "x".into(),
            interval: Duration::from_secs(1),
        },
        Arc::new(Src),
    )
    .unwrap();
    let failures = p.failures();
    p.start().unwrap();
    let t0 = Instant::now();
    while failures.load(SeqCst) == 0 {
        assert!(t0.elapsed() < Duration::from_secs(10), "no failure counted");
        std::thread::sleep(Duration::from_millis(20));
    }
    let t1 = Instant::now();
    p.stop();
    assert!(t1.elapsed() < Duration::from_secs(2));
}
