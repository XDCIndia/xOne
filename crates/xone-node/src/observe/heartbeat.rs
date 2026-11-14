//! SkyNet heartbeat (XB-21): a camelCase JSON POST on a timer.
//!
//! SkyNet silently drops heartbeats whose keys have the wrong case (issue #41), so the payload
//! is built by hand with fixed key names and checked by [`validate`]. The ingestion schema is
//! not in this repository: only `blockHeight`, `peerCount` and `isSyncing` are named by the
//! issue, so no other key is emitted (spec/ops/heartbeat.md).

use std::{
    fmt,
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{wire, ChainStats, PeerStats};
use crate::{
    lifecycle::{Service, ServiceError},
    TokenBucket,
};

#[derive(Clone)]
pub struct HeartbeatCfg {
    /// `https://host[:port]/path`. `http://` only with `allow_plaintext` (private devnet).
    pub url: String,
    pub allow_plaintext: bool,
    pub interval: Duration,
    /// Optional `Authorization` header value, from the env file. Never printed.
    pub authorization: Option<String>,
}

impl fmt::Debug for HeartbeatCfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeartbeatCfg")
            .field("url", &"<redacted>")
            .field("interval", &self.interval)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// `blockHeight` is null while the node has no head.
pub fn payload(chain: &dyn ChainStats, peers: &dyn PeerStats) -> String {
    let h = chain
        .head()
        .map_or("null".to_owned(), |b| b.number.to_string());
    format!(
        "{{\"blockHeight\":{h},\"peerCount\":{},\"isSyncing\":{}}}",
        peers.peer_count(),
        chain.syncing()
    )
}

/// The schema test: exactly these keys, these types, nothing else.
pub fn validate(body: &str) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let o = v.as_object().ok_or("not an object")?;
    if o.len() != 3 {
        return Err(format!(
            "expected 3 keys, got {:?}",
            o.keys().collect::<Vec<_>>()
        ));
    }
    let ok = |k: &str, f: fn(&serde_json::Value) -> bool| {
        o.get(k)
            .filter(|x| f(x))
            .map(|_| ())
            .ok_or(format!("missing or mistyped `{k}`"))
    };
    ok("blockHeight", |x| x.is_u64() || x.is_null())?;
    ok("peerCount", |x| x.is_u64())?;
    ok("isSyncing", |x| x.is_boolean())
}

/// One POST. Ok when the server answers 2xx.
pub fn post(cfg: &HeartbeatCfg, body: &str) -> Result<(), String> {
    let t = wire::parse_target(&cfg.url, ["https", "http"], "/")?;
    let (mut s, _) = wire::connect(&t, cfg.allow_plaintext, Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    let auth = cfg
        .authorization
        .as_ref()
        .map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        t.path,
        t.authority(),
        body.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut head = [0u8; 64];
    let mut n = 0;
    while n < 12 {
        match s.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = String::from_utf8_lossy(&head[..n]);
    match line.split(' ').nth(1) {
        Some(c) if c.starts_with('2') => Ok(()),
        _ => Err("heartbeat not accepted".into()),
    }
}

pub struct Heartbeat {
    cfg: HeartbeatCfg,
    chain: Arc<dyn ChainStats>,
    peers: Arc<dyn PeerStats>,
    stop: Arc<AtomicBool>,
    th: Option<JoinHandle<()>>,
}

impl Heartbeat {
    pub fn new(cfg: HeartbeatCfg, chain: Arc<dyn ChainStats>, peers: Arc<dyn PeerStats>) -> Self {
        Self {
            cfg,
            chain,
            peers,
            stop: Arc::default(),
            th: None,
        }
    }
}

impl Service for Heartbeat {
    fn name(&self) -> &str {
        "heartbeat"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let t = wire::parse_target(&self.cfg.url, ["https", "http"], "/").map_err(ServiceError)?;
        if !t.tls && !self.cfg.allow_plaintext {
            return Err(ServiceError(
                "plaintext heartbeat refused: use https or allow_plaintext on a private devnet"
                    .into(),
            ));
        }
        let (cfg, chain, peers, stop) = (
            self.cfg.clone(),
            self.chain.clone(),
            self.peers.clone(),
            self.stop.clone(),
        );
        let th = thread::Builder::new()
            .name("heartbeat".into())
            .spawn(move || {
                let t0 = Instant::now();
                // Sends are bounded to one burst of 3, then one per interval, so a bug in
                // the timer cannot hammer the server. Failures are not retried early.
                let secs = cfg.interval.as_secs().max(1);
                let mut bucket = TokenBucket::new(3, 1, 0);
                let mut next = Instant::now();
                while !stop.load(SeqCst) {
                    if Instant::now() >= next {
                        next += Duration::from_secs(secs);
                        let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if bucket.try_take(ms, secs) {
                            if let Err(e) = post(&cfg, &payload(&*chain, &*peers)) {
                                tracing::warn!(target: "xone_node::heartbeat", "{e}");
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
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
    use crate::observe::testutil::Fake;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
    };

    #[test]
    fn payload_matches_fixture_and_schema() {
        let f = Fake::new(12345);
        let body = payload(&f, &f);
        assert_eq!(
            body,
            include_str!("../../tests/fixtures/heartbeat/ok.json").trim()
        );
        validate(&body).unwrap();
        f.syncing.store(true, SeqCst);
        assert!(payload(&f, &f).ends_with("\"isSyncing\":true}"));
    }

    #[test]
    fn snake_case_and_wrong_types_fail() {
        assert!(validate("{\"block_height\":1,\"peer_count\":2,\"is_syncing\":false}").is_err());
        assert!(validate("{\"blockHeight\":1,\"peerCount\":2,\"is_syncing\":false}").is_err());
        assert!(validate("{\"blockHeight\":\"1\",\"peerCount\":2,\"isSyncing\":false}").is_err());
        assert!(
            validate("{\"blockHeight\":1,\"peerCount\":2,\"isSyncing\":false,\"extra\":1}")
                .is_err()
        );
        assert!(validate("{\"blockHeight\":null,\"peerCount\":0,\"isSyncing\":true}").is_ok());
    }

    #[test]
    fn posts_json_to_the_configured_url() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let (mut len, mut first) = (0usize, String::new());
            r.read_line(&mut first).unwrap();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length: ") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            (first, String::from_utf8(body).unwrap())
        });
        let f = Fake::new(7);
        let cfg = HeartbeatCfg {
            url: format!("http://127.0.0.1:{port}/hb"),
            allow_plaintext: true,
            interval: Duration::from_secs(60),
            authorization: Some("Bearer topsecret".into()),
        };
        assert!(!format!("{cfg:?}").contains("topsecret"));
        post(&cfg, &payload(&f, &f)).unwrap();
        let (first, body) = srv.join().unwrap();
        assert_eq!(first, "POST /hb HTTP/1.1\r\n");
        validate(&body).unwrap();
    }

    #[test]
    fn plaintext_needs_opt_in() {
        let f = Arc::new(Fake::new(1));
        let cfg = HeartbeatCfg {
            url: "http://127.0.0.1:1/".into(),
            allow_plaintext: false,
            interval: Duration::from_secs(1),
            authorization: None,
        };
        assert!(Heartbeat::new(cfg, f.clone(), f).start().is_err());
    }
}
