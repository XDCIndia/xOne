//! Metrics push to a Prometheus Pushgateway (issue #186, first slice).
//!
//! A pusher thread renders the same [`Snapshot`] as `/metrics` and sends it with one HTTP `PUT`
//! to `<url>/metrics/job/<job>` every interval. One request at a time, 5 s socket timeouts; a
//! failure is counted in `xone_metrics_push_failures_total` and never blocks the node. Plain
//! `http://` only, and no credentials in the URL (they would land in the config dump).

use std::{
    fmt::Write as _,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::metrics::{encode, MetricsSource};
use crate::lifecycle::{Service, ServiceError};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct PushCfg {
    /// `http://host:port[/prefix]` of the Pushgateway.
    pub url: String,
    pub job: String,
    pub interval: Duration,
}

/// Where one push goes: the socket address text (`host:port`) and the request path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub path: String,
}

/// Parses the push URL. Refuses another scheme, userinfo, a query and an empty or odd job.
pub fn target(url: &str, job: &str) -> Result<Target, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("metrics.push.url: only http:// is supported")?;
    if rest.contains('@') {
        return Err("metrics.push.url: credentials in the URL are not accepted".into());
    }
    if rest.contains('?') || rest.contains('#') {
        return Err("metrics.push.url: no query or fragment".into());
    }
    if job.is_empty()
        || !job
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err("metrics.push.job: use letters, digits, '_', '-' or '.'".into());
    }
    let (hostport, prefix) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    if hostport.is_empty() {
        return Err("metrics.push.url: no host".into());
    }
    let host = if hostport
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        hostport.to_owned()
    } else {
        format!("{hostport}:80")
    };
    Ok(Target {
        host,
        path: format!("{prefix}/metrics/job/{job}"),
    })
}

/// The pushed text: the `/metrics` page plus the pusher's own failure counter.
pub fn body(src: &dyn MetricsSource, failures: u64) -> String {
    let mut o = encode(&src.snapshot());
    let _ = writeln!(
        o,
        "# HELP xone_metrics_push_failures_total Metrics pushes that failed.\n# TYPE xone_metrics_push_failures_total counter\nxone_metrics_push_failures_total {failures}"
    );
    o
}

/// One `PUT`; Ok on a 2xx status.
pub fn push_once(t: &Target, body: &str) -> std::io::Result<()> {
    let addr = t
        .host
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other("no address for the push host"))?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    let req = format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        t.path,
        t.host,
        body.len()
    );
    s.write_all(req.as_bytes())?;
    s.write_all(body.as_bytes())?;
    let mut head = [0u8; 12];
    s.read_exact(&mut head)?;
    // "HTTP/1.1 2xx"
    if head.starts_with(b"HTTP/1.") && head.get(9) == Some(&b'2') {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "push refused: {}",
            String::from_utf8_lossy(&head)
        )))
    }
}

pub struct Pusher {
    target: Target,
    interval: Duration,
    src: Arc<dyn MetricsSource>,
    stop: Arc<AtomicBool>,
    failures: Arc<AtomicU64>,
    th: Option<JoinHandle<()>>,
}

impl Pusher {
    pub fn new(cfg: &PushCfg, src: Arc<dyn MetricsSource>) -> Result<Self, String> {
        Ok(Self {
            target: target(&cfg.url, &cfg.job)?,
            interval: cfg.interval.max(Duration::from_secs(1)),
            src,
            stop: Arc::default(),
            failures: Arc::default(),
            th: None,
        })
    }

    /// Failed pushes so far.
    pub fn failures(&self) -> Arc<AtomicU64> {
        self.failures.clone()
    }
}

impl Service for Pusher {
    fn name(&self) -> &str {
        "metrics-push"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let (t, every, src, stop, failures) = (
            self.target.clone(),
            self.interval,
            self.src.clone(),
            self.stop.clone(),
            self.failures.clone(),
        );
        let th = thread::Builder::new()
            .name("metrics-push".into())
            .spawn(move || {
                let mut next = Instant::now();
                while !stop.load(SeqCst) {
                    if Instant::now() >= next {
                        next = Instant::now() + every;
                        if let Err(e) = push_once(&t, &body(&*src, failures.load(SeqCst))) {
                            failures.fetch_add(1, SeqCst);
                            tracing::debug!(target: "metrics", "push to {}: {e}", t.host);
                        }
                    }
                    thread::sleep(Duration::from_millis(50));
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
