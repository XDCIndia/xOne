//! Logging (XB-21, ADR-007 section 6): a tracing layer with a byte budget per subsystem, a
//! global bytes/s cap, a reserved lane for ERROR lines, a bounded queue to one writer thread,
//! and a size-rotated file sink. The only global state in this crate is the subscriber that
//! [`Logs`] installs on `start`.

use std::{
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tracing::{field::Visit, Event, Level, Metadata, Subscriber};
use tracing_subscriber::{layer::Context, prelude::*, registry::Registry, Layer};

use crate::{
    lifecycle::{Service, ServiceError},
    TokenBucket,
};

/// ADR-007 Q17.
pub const QUEUE_LINES: usize = 8192;
const SUMMARY_EVERY_MS: u64 = 10_000;
const KIB: u64 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sub {
    Consensus,
    Exec,
    Net,
    Sync,
    Rpc,
    Db,
    Other,
}
const SUBS: [Sub; 7] = [
    Sub::Consensus,
    Sub::Exec,
    Sub::Net,
    Sub::Sync,
    Sub::Rpc,
    Sub::Db,
    Sub::Other,
];

impl Sub {
    fn of(target: &str) -> Self {
        let krate = target.split("::").next().unwrap_or("");
        match krate {
            "xone_consensus" | "xone_signer" => Sub::Consensus,
            "xone_executor" | "xone_evm" | "xone_state" | "xone_builder" => Sub::Exec,
            "xone_net" => Sub::Net,
            "xone_sync" => Sub::Sync,
            "xone_rpc" => Sub::Rpc,
            "xone_db" => Sub::Db,
            _ => Sub::Other,
        }
    }
    fn idx(self) -> usize {
        self as usize
    }
    fn name(self) -> &'static str {
        ["consensus", "exec", "net", "sync", "rpc", "db", "other"][self.idx()]
    }
    /// (sustained bytes/s, burst bytes), ADR-007 section 6 table.
    fn budget(self) -> (u64, u64) {
        match self {
            Sub::Consensus | Sub::Exec => (8 * KIB, 32 * KIB),
            Sub::Net | Sub::Sync | Sub::Rpc => (4 * KIB, 16 * KIB),
            Sub::Db | Sub::Other => (2 * KIB, 8 * KIB),
        }
    }
}

/// The budgets. Pure: callers pass milliseconds.
pub struct Limiter {
    subs: Vec<TokenBucket>,
    global: TokenBucket,
    alarm: TokenBucket,
    suppressed: [(u64, u64); 7], // (lines, bytes) since the last summary
    last_summary: [u64; 7],
}

impl Limiter {
    pub fn new(now_ms: u64) -> Self {
        Self {
            subs: SUBS
                .iter()
                .map(|s| TokenBucket::new(s.budget().1, s.budget().0, now_ms))
                .collect(),
            global: TokenBucket::new(128 * KIB, 32 * KIB, now_ms),
            alarm: TokenBucket::new(16 * KIB, 4 * KIB, now_ms),
            suppressed: [(0, 0); 7],
            last_summary: [now_ms; 7],
        }
    }

    /// Whether a line of `len` bytes may be written, and a summary line to write first when
    /// one is due. ERROR lines draw only from the alarm lane, so a flood of INFO cannot hide
    /// them and they do not count against the global bucket (ADR-007: the total bound is the
    /// global cap plus the lane).
    fn offer(&mut self, now_ms: u64, sub: Sub, alarm: bool, len: u64) -> (bool, Option<String>) {
        let i = sub.idx();
        let pass = if alarm {
            self.alarm.try_take(now_ms, len)
        } else {
            self.subs[i].try_take(now_ms, len) && self.global.try_take(now_ms, len)
        };
        if !pass {
            self.suppressed[i].0 += 1;
            self.suppressed[i].1 += len;
            return (false, None);
        }
        let (lines, bytes) = self.suppressed[i];
        if lines > 0 && now_ms.saturating_sub(self.last_summary[i]) >= SUMMARY_EVERY_MS {
            let s = format!(
                "suppressed lines={lines} bytes={bytes} subsystem={}\n",
                sub.name()
            );
            if self.alarm.try_take(now_ms, s.len() as u64) {
                self.suppressed[i] = (0, 0);
                self.last_summary[i] = now_ms;
                return (true, Some(s));
            }
        }
        (true, None)
    }

    /// Summaries still owed, written at shutdown.
    fn drain(&mut self) -> Vec<String> {
        let mut v = vec![];
        for s in SUBS {
            let (l, b) = std::mem::take(&mut self.suppressed[s.idx()]);
            if l > 0 {
                v.push(format!(
                    "suppressed lines={l} bytes={b} subsystem={}\n",
                    s.name()
                ));
            }
        }
        v
    }
}

// ---- levels ----

/// geth verbosity: 0 silent, 1 error, 2 warn, 3 info, 4 debug, 5 trace.
fn allows(verbosity: u8, level: &Level) -> bool {
    let max = match verbosity {
        0 => return false,
        1 => Level::ERROR,
        2 => Level::WARN,
        3 => Level::INFO,
        4 => Level::DEBUG,
        _ => Level::TRACE,
    };
    *level <= max // tracing orders ERROR < WARN < INFO < DEBUG < TRACE
}

#[derive(Clone, Debug)]
pub struct LevelCfg {
    pub verbosity: u8,
    /// Per-module verbosity by target prefix, e.g. `("xone_net", 4)`; longest prefix wins.
    pub modules: Vec<(String, u8)>,
}

impl LevelCfg {
    fn allows(&self, m: &Metadata<'_>) -> bool {
        let t = m.target();
        let v = self
            .modules
            .iter()
            .filter(|(p, _)| {
                t == p
                    || t.strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with("::"))
            })
            .max_by_key(|(p, _)| p.len())
            .map_or(self.verbosity, |(_, v)| *v);
        allows(v, m.level())
    }
}

// ---- sinks ----

/// `<path>`, then `<path>.1` (newest) to `<path>.<files-1>` (oldest).
pub struct Rotating {
    path: PathBuf,
    max: u64,
    files: usize,
    f: File,
    size: u64,
}

impl Rotating {
    pub fn open(path: PathBuf, max_bytes: u64, files: usize) -> io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = f.metadata()?.len();
        Ok(Self {
            path,
            max: max_bytes,
            files: files.max(1),
            f,
            size,
        })
    }

    fn numbered(&self, n: usize) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(format!(".{n}"));
        s.into()
    }

    fn rotate(&mut self) -> io::Result<()> {
        if self.files <= 1 {
            fs::remove_file(&self.path)?;
        } else {
            let _ = fs::remove_file(self.numbered(self.files - 1));
            for n in (1..self.files - 1).rev() {
                if self.numbered(n).exists() {
                    fs::rename(self.numbered(n), self.numbered(n + 1))?;
                }
            }
            fs::rename(&self.path, self.numbered(1))?;
        }
        self.f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for Rotating {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + b.len() as u64 > self.max {
            self.rotate()?;
        }
        self.f.write_all(b)?;
        self.size += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.f.flush()
    }
}

/// `Stdout` is for a supervisor (journald): the unit must set its own rate limit.
#[derive(Clone, Debug)]
pub enum SinkCfg {
    Stdout,
    File {
        path: PathBuf,
        max_bytes: u64,
        files: usize,
    },
}

impl SinkCfg {
    /// ADR-007: 64 MiB x 8.
    pub fn file(path: PathBuf) -> Self {
        Self::File {
            path,
            max_bytes: 64 * 1024 * 1024,
            files: 8,
        }
    }
    fn open(&self) -> io::Result<Box<dyn Write + Send>> {
        Ok(match self {
            Self::Stdout => Box::new(io::stdout()),
            Self::File {
                path,
                max_bytes,
                files,
            } => Box::new(Rotating::open(path.clone(), *max_bytes, *files)?),
        })
    }
}

// ---- the layer ----

enum Msg {
    Line(String),
    Flush(SyncSender<()>),
}

type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

struct Inner {
    levels: LevelCfg,
    limiter: Mutex<Limiter>,
    tx: SyncSender<Msg>,
    clock: Clock,
    suppressed: AtomicU64,
    dropped: AtomicU64,
}

#[derive(Clone)]
pub struct LogLayer(Arc<Inner>);

#[derive(Default)]
struct Fields {
    msg: String,
    rest: String,
}
impl Visit for Fields {
    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
        if f.name() == "message" {
            self.msg.push_str(v);
        } else {
            let _ = write!(self.rest, " {}={v}", f.name());
        }
    }
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
        if f.name() == "message" {
            let _ = write!(self.msg, "{v:?}");
        } else {
            let _ = write!(self.rest, " {}={v:?}", f.name());
        }
    }
}

impl LogLayer {
    fn send(&self, line: String) {
        match self.0.tx.try_send(Msg::Line(line)) {
            Ok(()) => {}
            // Q17: reject the newest, counted; hot threads never block on logging.
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.0.dropped.fetch_add(1, Relaxed);
            }
        }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn enabled(&self, m: &Metadata<'_>, _: Context<'_, S>) -> bool {
        self.0.levels.allows(m)
    }

    fn on_event(&self, ev: &Event<'_>, _: Context<'_, S>) {
        let m = ev.metadata();
        let mut f = Fields::default();
        ev.record(&mut f);
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let line = format!(
            "{wall} {:<5} {} {}{}\n",
            m.level(),
            m.target(),
            f.msg,
            f.rest
        );
        let now = (self.0.clock)();
        let (pass, summary) = {
            let mut l = self.0.limiter.lock().unwrap_or_else(|e| e.into_inner());
            l.offer(
                now,
                Sub::of(m.target()),
                *m.level() == Level::ERROR,
                line.len() as u64,
            )
        };
        if !pass {
            self.0.suppressed.fetch_add(1, Relaxed);
            return;
        }
        if let Some(s) = summary {
            self.send(s);
        }
        self.send(line);
    }
}

/// Handle on the writer thread; the `Service` installs the subscriber.
pub struct Logs {
    layer: LogLayer,
    rx: Option<Receiver<Msg>>,
    sink: Option<Box<dyn Write + Send>>,
    th: Option<JoinHandle<()>>,
    install: bool,
}

impl Logs {
    /// Opens the sink. `install` false builds the layer only (tests).
    pub fn new(levels: LevelCfg, sink: &SinkCfg) -> io::Result<Self> {
        let t0 = Instant::now();
        let clock = Box::new(move || u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX));
        Ok(Self::with(levels, sink.open()?, clock, true))
    }

    fn with(levels: LevelCfg, sink: Box<dyn Write + Send>, clock: Clock, install: bool) -> Self {
        let (tx, rx) = sync_channel(QUEUE_LINES);
        let now = clock();
        let layer = LogLayer(Arc::new(Inner {
            levels,
            limiter: Mutex::new(Limiter::new(now)),
            tx,
            clock,
            suppressed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }));
        Self {
            layer,
            rx: Some(rx),
            sink: Some(sink),
            th: None,
            install,
        }
    }

    /// (lines suppressed by the budgets, lines dropped by a full queue), for the metrics.
    pub fn counters(&self) -> (u64, u64) {
        (
            self.layer.0.suppressed.load(Relaxed),
            self.layer.0.dropped.load(Relaxed),
        )
    }

    pub fn layer(&self) -> LogLayer {
        self.layer.clone()
    }

    /// Writes the owed summaries and waits (bounded) until everything queued is written.
    pub fn flush(&self) {
        let owed = self
            .layer
            .0
            .limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain();
        for s in owed {
            self.layer.send(s);
        }
        let (ack_tx, ack_rx) = sync_channel(1);
        if send_bounded(&self.layer.0.tx, Msg::Flush(ack_tx)) {
            let _ = ack_rx.recv_timeout(Duration::from_secs(1));
        }
    }
}

/// std has no send_timeout for sync_channel: poll try_send for up to a second.
fn send_bounded(tx: &SyncSender<Msg>, mut m: Msg) -> bool {
    for _ in 0..100 {
        match tx.try_send(m) {
            Ok(()) => return true,
            Err(TrySendError::Full(x)) => {
                m = x;
                thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    false
}

impl Service for Logs {
    fn name(&self) -> &str {
        "logs"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let (Some(rx), Some(mut sink)) = (self.rx.take(), self.sink.take()) else {
            return Err(ServiceError("logs already started".into()));
        };
        self.th = Some(
            thread::Builder::new()
                .name("log-writer".into())
                .spawn(move || {
                    while let Ok(m) = rx.recv() {
                        match m {
                            Msg::Line(l) => {
                                let _ = sink.write_all(l.as_bytes());
                            }
                            Msg::Flush(ack) => {
                                let _ = sink.flush();
                                let _ = ack.send(());
                            }
                        }
                    }
                })
                .map_err(|e| ServiceError(e.to_string()))?,
        );
        if self.install {
            tracing::subscriber::set_global_default(Registry::default().with(self.layer()))
                .map_err(|e| ServiceError(format!("a tracing subscriber is already set: {e}")))?;
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

    #[derive(Clone, Default)]
    struct Mem(Arc<Mutex<Vec<u8>>>);
    impl Write for Mem {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn lv(v: u8) -> LevelCfg {
        LevelCfg {
            verbosity: v,
            modules: vec![],
        }
    }

    /// One simulated hour, one 200-byte line per millisecond rotating over every subsystem:
    /// the bytes passed stay within the global cap times elapsed time plus the burst.
    #[test]
    fn flood_is_bounded_by_the_global_cap() {
        let mut l = Limiter::new(0);
        let (mut bytes, mut total) = (0u64, 0u64);
        for t in 0..3_600_000u64 {
            let sub = SUBS[(t % 7) as usize];
            let (pass, sum) = l.offer(t, sub, false, 200);
            if pass {
                bytes += 200;
            }
            if let Some(s) = sum {
                total += s.len() as u64;
            }
        }
        assert!(bytes <= 128 * KIB + 32 * KIB * 3600, "{bytes}");
        assert!(
            bytes >= 32 * KIB * 3600 - 200 * 8,
            "limiter starves: {bytes}"
        ); // and it is not under-delivering
           // Summaries: one per subsystem per 10 s at most, drawn from the alarm lane.
        assert!(total <= 16 * KIB + 4 * KIB * 3600);
    }

    #[test]
    fn error_lane_survives_an_info_flood() {
        let mut l = Limiter::new(0);
        for _ in 0..100_000 {
            l.offer(0, Sub::Net, false, 200);
        }
        assert!(!l.offer(0, Sub::Net, false, 200).0);
        assert!(l.offer(0, Sub::Net, true, 200).0);
    }

    fn run(levels: LevelCfg, clock: &Arc<AtomicU64>, f: impl FnOnce()) -> (String, (u64, u64)) {
        let mem = Mem::default();
        let c = clock.clone();
        let mut logs = Logs::with(
            levels,
            Box::new(mem.clone()),
            Box::new(move || c.load(SeqCst)),
            false,
        );
        logs.start().unwrap();
        tracing::subscriber::with_default(Registry::default().with(logs.layer()), f);
        logs.stop();
        let out = String::from_utf8(mem.0.lock().unwrap().clone()).unwrap();
        (out, logs.counters())
    }

    #[test]
    fn layer_bounds_a_flood_summarises_and_keeps_errors() {
        let clock = Arc::new(AtomicU64::new(0));
        let c2 = clock.clone();
        let (out, (suppressed, dropped)) = run(lv(3), &clock, || {
            for i in 0..10_000 {
                tracing::info!(target: "xone_net::peer", i, "peer message");
            }
            tracing::error!(target: "xone_net::peer", "alarm");
            c2.store(10_000, SeqCst);
            tracing::info!(target: "xone_net::peer", "later");
        });
        let flood: usize = out
            .lines()
            .filter(|l| l.contains("peer message"))
            .map(|l| l.len() + 1)
            .sum();
        assert!(flood <= 16 * 1024, "{flood}"); // the net burst, clock frozen
        assert!(out.contains(" ERROR xone_net::peer alarm"));
        assert!(
            out.contains("suppressed lines=") && out.contains("subsystem=net"),
            "{out}"
        );
        assert!(out.contains("later"));
        assert!(suppressed > 9_000 && dropped == 0);
    }

    #[test]
    fn verbosity_and_module_levels() {
        let c = LevelCfg {
            verbosity: 3,
            modules: vec![("xone_net".into(), 5), ("xone_net::peer".into(), 1)],
        };
        let (out, _) = run(c, &Arc::new(AtomicU64::new(0)), || {
            tracing::debug!(target: "xone_net::sync", "a-net-debug");
            tracing::info!(target: "xone_net::peer", "b-peer-info"); // longest prefix: error only
            tracing::debug!(target: "xone_db", "c-db-debug"); // global 3: no debug
            tracing::info!(target: "xone_db", "d-db-info");
            tracing::info!(target: "xone_network", "e-prefix-is-not-a-module-match");
        });
        assert!(
            out.contains("a-net-debug") && out.contains("d-db-info") && out.contains("e-prefix")
        );
        assert!(!out.contains("b-peer-info") && !out.contains("c-db-debug"));
        assert!(allows(3, &Level::INFO) && !allows(3, &Level::DEBUG) && !allows(0, &Level::ERROR));
    }

    #[test]
    fn rotation_keeps_at_most_n_files_of_at_most_max_bytes() {
        let dir = std::env::temp_dir().join(format!("xone-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("xone.log");
        let mut r = Rotating::open(p.clone(), 100, 3).unwrap();
        for i in 0..50 {
            r.write_all(format!("{i:029}\n").as_bytes()).unwrap();
        }
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["xone.log", "xone.log.1", "xone.log.2"]);
        for n in &names {
            assert!(fs::metadata(dir.join(n)).unwrap().len() <= 100);
        }
        assert!(fs::read_to_string(&p)
            .unwrap()
            .contains("00000000000000000000000000049"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// ADR-007 rule 1: no INFO on a per-message path. The peer, sync and pool sources must not
    /// call `info!`; per-message events log at debug or trace.
    #[test]
    fn no_info_on_per_message_paths() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = vec![];
        for d in [
            "xone-net/src",
            "xone-sync/src",
            "xone-builder/src/txpool",
            "xone-rpc/src",
        ] {
            let mut stack = vec![root.join(d)];
            while let Some(p) = stack.pop() {
                let Ok(rd) = fs::read_dir(&p) else { continue };
                for e in rd.flatten() {
                    let q = e.path();
                    if q.is_dir() {
                        stack.push(q);
                    } else if q.extension().is_some_and(|x| x == "rs") {
                        files.push(q);
                    }
                }
            }
        }
        assert!(
            files.len() > 5,
            "hot-path sources not found: the test would pass vacuously"
        );
        let mut bad = vec![];
        for f in files {
            for (n, l) in fs::read_to_string(&f).unwrap().lines().enumerate() {
                let code = l.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                let mut at = 0;
                while let Some(i) = code[at..].find("info!(") {
                    let j = at + i;
                    let prev = code[..j].chars().last();
                    if !prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        bad.push(format!("{}:{}", f.display(), n + 1));
                    }
                    at = j + 1;
                }
            }
        }
        assert!(bad.is_empty(), "INFO on a per-message path: {bad:?}");
    }
}
