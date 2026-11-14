//! Prometheus text exposition (format 0.0.4) and the health endpoints (XB-21).
//!
//! The node fills a [`Snapshot`] from its own counters; this module only formats and serves
//! it, on its own thread with short socket timeouts, so a slow scraper cannot stall the node.
//! `/metrics`, `/livez` (always 200 while the process runs) and `/readyz` (200 only when
//! [`Snapshot::ready`]).

use std::{
    fmt::Write as _,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use super::hex0x;
use crate::lifecycle::{Service, ServiceError};

#[derive(Clone, Debug, Default)]
pub struct QueueStat {
    /// ADR-007 queue id, "Q1" to "Q18".
    pub id: &'static str,
    pub depth: u64,
    pub overflow_total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// The one build stamp.
    pub version: String,
    pub head_number: Option<u64>,
    pub head_hash: [u8; 32],
    /// Unix seconds of the head block.
    pub head_timestamp: u64,
    /// Unix seconds now, from the caller's clock.
    pub now: u64,
    pub syncing: bool,
    pub peers: u64,
    /// Peers dropped for another hash (or no answer) at a required block.
    pub peers_required_block_refused_total: u64,
    pub txpool_pending: u64,
    pub txpool_queued: u64,
    pub blocks_imported_total: u64,
    /// (class "c1".."c3" per ADR-007 section 4, running requests).
    pub rpc_in_flight: Vec<(&'static str, u64)>,
    pub queues: Vec<QueueStat>,
    /// How late the watchdog tick ran, milliseconds.
    pub watchdog_lag_ms: u64,
    pub consensus_round: u64,
    pub highest_qc_round: u64,
    pub timeouts_total: u64,
    pub missed_eligible_turns_total: u64,
    pub storage_bytes: u64,
    pub log_suppressed_lines_total: u64,
    pub log_dropped_lines_total: u64,
    /// Durability or signer-fencing fault, or shutdown in progress: never ready.
    pub degraded: bool,
    pub shutting_down: bool,
}

impl Snapshot {
    pub fn head_age_secs(&self) -> Option<u64> {
        self.head_number
            .map(|_| self.now.saturating_sub(self.head_timestamp))
    }

    /// Ready: has a head, is not syncing, the head is fresh, nothing degraded, not stopping.
    /// A header-only or pivot sync reports `syncing`; a stale verified head fails the age test.
    pub fn ready(&self, max_head_age_secs: u64) -> bool {
        self.head_age_secs().is_some_and(|a| a <= max_head_age_secs)
            && !self.syncing
            && !self.degraded
            && !self.shutting_down
    }
}

fn esc(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn family(o: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(o, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

pub fn encode(s: &Snapshot) -> String {
    let mut o = String::new();
    let g = |o: &mut String, n: &str, kind: &str, help: &str, v: u64| {
        family(o, n, kind, help);
        let _ = writeln!(o, "{n} {v}");
    };
    family(&mut o, "xone_build_info", "gauge", "Build stamp; always 1.");
    let _ = writeln!(o, "xone_build_info{{version=\"{}\"}} 1", esc(&s.version));
    if let Some(n) = s.head_number {
        g(
            &mut o,
            "xone_head_block_number",
            "gauge",
            "Number of the head block.",
            n,
        );
        family(
            &mut o,
            "xone_head_block_hash_info",
            "gauge",
            "Head block hash as a label; always 1.",
        );
        let _ = writeln!(
            o,
            "xone_head_block_hash_info{{hash=\"{}\"}} 1",
            hex0x(&s.head_hash)
        );
        g(
            &mut o,
            "xone_head_block_age_seconds",
            "gauge",
            "Seconds since the head block timestamp.",
            s.head_age_secs().unwrap_or(0),
        );
    }
    g(
        &mut o,
        "xone_syncing",
        "gauge",
        "1 while syncing.",
        u64::from(s.syncing),
    );
    g(&mut o, "xone_peers", "gauge", "Connected peers.", s.peers);
    g(
        &mut o,
        "xone_peers_required_block_refused_total",
        "counter",
        "Peers disconnected for another hash at a required block (--eth.requiredblocks).",
        s.peers_required_block_refused_total,
    );
    g(
        &mut o,
        "xone_txpool_pending",
        "gauge",
        "Executable transactions in the pool.",
        s.txpool_pending,
    );
    g(
        &mut o,
        "xone_txpool_queued",
        "gauge",
        "Non-executable transactions in the pool.",
        s.txpool_queued,
    );
    g(
        &mut o,
        "xone_blocks_imported_total",
        "counter",
        "Blocks imported; rate() gives the import rate.",
        s.blocks_imported_total,
    );
    family(
        &mut o,
        "xone_rpc_in_flight",
        "gauge",
        "Running RPC requests per ADR-007 class.",
    );
    for (c, n) in &s.rpc_in_flight {
        let _ = writeln!(o, "xone_rpc_in_flight{{class=\"{}\"}} {n}", esc(c));
    }
    family(
        &mut o,
        "xone_queue_depth",
        "gauge",
        "Items in each ADR-007 queue.",
    );
    for q in &s.queues {
        let _ = writeln!(o, "xone_queue_depth{{queue=\"{}\"}} {}", esc(q.id), q.depth);
    }
    family(
        &mut o,
        "xone_queue_overflow_total",
        "counter",
        "Overflow actions per ADR-007 queue.",
    );
    for q in &s.queues {
        let _ = writeln!(
            o,
            "xone_queue_overflow_total{{queue=\"{}\"}} {}",
            esc(q.id),
            q.overflow_total
        );
    }
    family(
        &mut o,
        "xone_watchdog_lag_seconds",
        "gauge",
        "Lateness of the watchdog tick.",
    );
    let _ = writeln!(
        o,
        "xone_watchdog_lag_seconds {}.{:03}",
        s.watchdog_lag_ms / 1000,
        s.watchdog_lag_ms % 1000
    );
    g(
        &mut o,
        "xone_consensus_round",
        "gauge",
        "Current XDPoS v2 round.",
        s.consensus_round,
    );
    g(
        &mut o,
        "xone_highest_qc_round",
        "gauge",
        "Round of the highest quorum certificate.",
        s.highest_qc_round,
    );
    g(
        &mut o,
        "xone_timeouts_total",
        "counter",
        "Round timeouts.",
        s.timeouts_total,
    );
    g(
        &mut o,
        "xone_missed_eligible_turns_total",
        "counter",
        "Eligible mining turns that produced no block from this node.",
        s.missed_eligible_turns_total,
    );
    g(
        &mut o,
        "xone_storage_bytes",
        "gauge",
        "Bytes used by the datadir.",
        s.storage_bytes,
    );
    g(
        &mut o,
        "xone_log_suppressed_lines_total",
        "counter",
        "Log lines dropped by the rate limits.",
        s.log_suppressed_lines_total,
    );
    g(
        &mut o,
        "xone_log_dropped_lines_total",
        "counter",
        "Log lines dropped by a full sink queue.",
        s.log_dropped_lines_total,
    );
    o
}

pub trait MetricsSource: Send + Sync {
    fn snapshot(&self) -> Snapshot;
}

#[derive(Clone, Debug)]
pub struct MetricsCfg {
    pub addr: String,
    pub port: u16,
    pub max_head_age_secs: u64,
}

fn reply(s: &mut TcpStream, code: &str, ctype: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn serve_one(mut s: TcpStream, src: &dyn MetricsSource, max_age: u64) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 2048];
    let mut n = 0;
    // Only the request line matters; stop at the first newline or a full buffer.
    while n < buf.len() && !buf[..n].contains(&b'\n') {
        match s.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let line = String::from_utf8_lossy(&buf[..n]);
    let mut p = line.split(' ');
    let (method, path) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
    let text = "text/plain; version=0.0.4; charset=utf-8";
    match (method, path) {
        ("GET", "/metrics") => reply(&mut s, "200 OK", text, &encode(&src.snapshot())),
        ("GET", "/livez") => reply(&mut s, "200 OK", "text/plain", "ok\n"),
        ("GET", "/readyz") => {
            if src.snapshot().ready(max_age) {
                reply(&mut s, "200 OK", "text/plain", "ready\n")
            } else {
                reply(
                    &mut s,
                    "503 Service Unavailable",
                    "text/plain",
                    "not ready\n",
                )
            }
        }
        _ => reply(&mut s, "404 Not Found", "text/plain", "not found\n"),
    }
}

pub struct MetricsServer {
    cfg: MetricsCfg,
    src: Arc<dyn MetricsSource>,
    stop: Arc<AtomicBool>,
    th: Option<JoinHandle<()>>,
    bound: Option<std::net::SocketAddr>,
}

impl MetricsServer {
    pub fn new(cfg: MetricsCfg, src: Arc<dyn MetricsSource>) -> Self {
        Self {
            cfg,
            src,
            stop: Arc::default(),
            th: None,
            bound: None,
        }
    }

    /// The address actually bound (after `start`), for port 0.
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.bound
    }
}

impl Service for MetricsServer {
    fn name(&self) -> &str {
        "metrics"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let l = TcpListener::bind((self.cfg.addr.as_str(), self.cfg.port))
            .map_err(|e| ServiceError(format!("metrics bind: {e}")))?;
        l.set_nonblocking(true)
            .map_err(|e| ServiceError(e.to_string()))?;
        self.bound = l.local_addr().ok();
        let (src, stop, max_age) = (
            self.src.clone(),
            self.stop.clone(),
            self.cfg.max_head_age_secs,
        );
        let th = thread::Builder::new()
            .name("metrics".into())
            .spawn(move || {
                while !stop.load(SeqCst) {
                    match l.accept() {
                        // One request at a time with 2 s socket timeouts: a scraper cannot
                        // hold more than that.
                        Ok((s, _)) => serve_one(s, &*src, max_age),
                        Err(_) => thread::sleep(Duration::from_millis(50)),
                    }
                }
            })
            .map_err(|e| ServiceError(e.to_string()))?;
        self.th = Some(th);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, sync::atomic::AtomicU64};

    /// Minimal text-format 0.0.4 checker: valid names, TYPE before samples, parseable values,
    /// quoted and escaped label values, counters named `_total`.
    pub(crate) fn check(text: &str) -> Result<Vec<(String, String, f64)>, String> {
        let name_ok = |n: &str| {
            let mut c = n.chars();
            c.next()
                .is_some_and(|f| f.is_ascii_alphabetic() || f == '_' || f == ':')
                && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || x == ':')
        };
        let (mut typed, mut out) = (HashSet::new(), vec![]);
        for l in text.lines() {
            if let Some(r) = l.strip_prefix("# TYPE ") {
                let (n, k) = r.split_once(' ').ok_or("bad TYPE")?;
                if !name_ok(n) || !["gauge", "counter"].contains(&k) {
                    return Err(format!("bad TYPE line {l}"));
                }
                if k == "counter" && !n.ends_with("_total") {
                    return Err(format!("counter without _total: {n}"));
                }
                typed.insert(n.to_owned());
            } else if l.starts_with("# HELP ") {
            } else if l.starts_with('#') || l.is_empty() {
                return Err(format!("unexpected line {l:?}"));
            } else {
                let (series, v) = l.rsplit_once(' ').ok_or("no value")?;
                let v: f64 = v.parse().map_err(|_| format!("bad value in {l}"))?;
                let (n, labels) = match series.split_once('{') {
                    Some((n, r)) => (n, r.strip_suffix('}').ok_or("unclosed labels")?),
                    None => (series, ""),
                };
                if !name_ok(n) || !typed.contains(n) {
                    return Err(format!("sample without TYPE: {n}"));
                }
                if !labels.is_empty() {
                    let (k, val) = labels.split_once('=').ok_or("bad label")?;
                    if !name_ok(k) || !val.starts_with('"') || !val.ends_with('"') || val.len() < 2
                    {
                        return Err(format!("bad label {labels}"));
                    }
                }
                out.push((n.to_owned(), labels.to_owned(), v));
            }
        }
        Ok(out)
    }

    fn snap() -> Snapshot {
        Snapshot {
            version: "xone/v0.1.0-abc1234 \"x\"".into(),
            head_number: Some(100),
            head_hash: [0xab; 32],
            head_timestamp: 1000,
            now: 1004,
            peers: 7,
            rpc_in_flight: vec![("c1", 1), ("c2", 2), ("c3", 0)],
            queues: vec![
                QueueStat {
                    id: "Q2",
                    depth: 5,
                    overflow_total: 1,
                },
                QueueStat {
                    id: "Q17",
                    depth: 0,
                    overflow_total: 0,
                },
            ],
            watchdog_lag_ms: 1250,
            ..Default::default()
        }
    }

    #[test]
    fn exposition_parses_and_lists_the_named_metrics() {
        let text = encode(&snap());
        let m = check(&text).unwrap();
        for want in [
            "xone_build_info",
            "xone_head_block_number",
            "xone_head_block_hash_info",
            "xone_head_block_age_seconds",
            "xone_peers",
            "xone_peers_required_block_refused_total",
            "xone_txpool_pending",
            "xone_txpool_queued",
            "xone_blocks_imported_total",
            "xone_rpc_in_flight",
            "xone_queue_depth",
            "xone_queue_overflow_total",
            "xone_watchdog_lag_seconds",
            "xone_consensus_round",
            "xone_highest_qc_round",
            "xone_timeouts_total",
            "xone_missed_eligible_turns_total",
            "xone_storage_bytes",
        ] {
            assert!(m.iter().any(|(n, _, _)| n == want), "missing {want}");
        }
        let v = |n: &str| m.iter().find(|(x, _, _)| x == n).map(|x| x.2).unwrap();
        assert_eq!(v("xone_head_block_age_seconds"), 4.0);
        assert_eq!(v("xone_watchdog_lag_seconds"), 1.25);
        assert!(text.contains("xone_build_info{version=\"xone/v0.1.0-abc1234 \\\"x\\\"\"} 1"));
        assert!(check("xone_x 1\n").is_err());
        assert!(check("# TYPE a_x counter\na_x 1\n").is_err());
    }

    #[test]
    fn readiness_rules() {
        let mut s = snap();
        assert!(s.ready(60));
        s.now = 2000; // stale head
        assert!(!s.ready(60));
        let mut s = snap();
        s.syncing = true;
        assert!(!s.ready(60));
        let mut s = snap();
        s.degraded = true;
        assert!(!s.ready(60));
        let mut s = snap();
        s.head_number = None;
        assert!(!s.ready(60));
        s = snap();
        s.shutting_down = true;
        assert!(!s.ready(60));
    }

    struct Src(AtomicU64, AtomicBool);
    impl MetricsSource for Src {
        fn snapshot(&self) -> Snapshot {
            Snapshot {
                missed_eligible_turns_total: self.0.load(SeqCst),
                syncing: self.1.load(SeqCst),
                ..snap()
            }
        }
    }

    fn get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = String::new();
        s.read_to_string(&mut r).unwrap();
        let (h, b) = r.split_once("\r\n\r\n").unwrap();
        (h.lines().next().unwrap().to_owned(), b.to_owned())
    }

    #[test]
    fn scrape_missed_turn_and_health_endpoints() {
        let src = Arc::new(Src(AtomicU64::new(0), AtomicBool::new(true)));
        let mut srv = MetricsServer::new(
            MetricsCfg {
                addr: "127.0.0.1".into(),
                port: 0,
                max_head_age_secs: 60,
            },
            src.clone(),
        );
        srv.start().unwrap();
        let a = srv.local_addr().unwrap();
        let missed = |a| {
            let (st, body) = get(a, "/metrics");
            assert!(st.contains("200"));
            check(&body)
                .unwrap()
                .into_iter()
                .find(|(n, _, _)| n == "xone_missed_eligible_turns_total")
                .unwrap()
                .2
        };
        assert_eq!(missed(a), 0.0);
        src.0.fetch_add(1, SeqCst); // a simulated miss
        assert_eq!(missed(a), 1.0);
        assert!(get(a, "/readyz").0.contains("503")); // syncing
        src.1.store(false, SeqCst);
        assert!(get(a, "/readyz").0.contains("200")); // at tip
        assert!(get(a, "/livez").0.contains("200"));
        assert!(get(a, "/nope").0.contains("404"));
        srv.stop();
    }
}
