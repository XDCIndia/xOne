//! Bounded node lifecycle: services start in order and stop in reverse, each with a
//! deadline, so a service that blocks cannot make shutdown hang (xOneGo#703).

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServiceError(pub String);

pub trait Service: Send {
    fn name(&self) -> &str;
    fn start(&mut self) -> Result<(), ServiceError>;
    /// May block; the node gives up on it at its deadline.
    fn stop(&mut self);
}

/// Why the node did not exit cleanly.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeError {
    #[error("service `{service}` failed to start: {reason}")]
    StartFailed { service: String, reason: String },
    #[error("services did not stop within their deadline: {0:?}")]
    StopTimeout(Vec<String>),
    #[error("the datadir filesystem is out of free space (disk watermark exit line)")]
    DiskFull,
}

impl NodeError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::StartFailed { .. } => 1,
            Self::StopTimeout(_) => 3,
            Self::DiskFull => 4,
        }
    }
}

/// Stop requests from signals or callers. Only the first one counts.
#[derive(Clone, Default)]
pub struct Shutdown(Arc<AtomicBool>);

impl Shutdown {
    /// True for the first request only; later ones are no-ops.
    pub fn request(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// SIGTERM and SIGINT request a stop. The handlers stay installed, so a second
    /// signal during the stop does not kill the process or start another stop.
    pub fn install_signals(&self) -> std::io::Result<()> {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])?;
        let me = self.clone();
        thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                for _ in signals.forever() {
                    me.request();
                }
            })
            .map(|_| ())
    }
}

pub struct Node {
    services: Vec<Box<dyn Service>>,
    started: usize,
    stopped: bool,
    /// Asked by a service (the disk guard), as opposed to the operator's signal.
    internal_stop: Shutdown,
    /// Asked by a service whose work is done (`--exitwhensynced`): a clean stop, exit 0.
    finished: Shutdown,
    per_service: Duration,
    total: Duration,
}

impl Node {
    /// `per_service` bounds one stop, `total` the whole shutdown.
    pub fn new(per_service: Duration, total: Duration) -> Self {
        Self {
            services: vec![],
            started: 0,
            stopped: false,
            internal_stop: Shutdown::default(),
            finished: Shutdown::default(),
            per_service,
            total,
        }
    }

    /// A handle a service can use to stop the node: `run` then stops and returns [`NodeError::DiskFull`].
    pub fn internal_stop(&self) -> Shutdown {
        self.internal_stop.clone()
    }

    /// A handle a service can use to end the run cleanly: `run` then stops and returns `Ok`.
    pub fn finished(&self) -> Shutdown {
        self.finished.clone()
    }

    pub fn add(&mut self, s: Box<dyn Service>) {
        self.services.push(s);
    }

    /// Start in order. On a failure the services already started are stopped.
    pub fn start(&mut self) -> Result<(), NodeError> {
        for i in 0..self.services.len() {
            if let Err(e) = self.services[i].start() {
                let service = self.services[i].name().to_owned();
                let _ = self.stop();
                return Err(NodeError::StartFailed {
                    service,
                    reason: e.0,
                });
            }
            self.started = i + 1;
        }
        Ok(())
    }

    /// Stop the started services in reverse. Runs once; later calls return Ok.
    pub fn stop(&mut self) -> Result<(), NodeError> {
        if std::mem::replace(&mut self.stopped, true) {
            return Ok(());
        }
        self.services.truncate(self.started);
        let begin = Instant::now();
        let mut late = vec![];
        while let Some(mut s) = self.services.pop() {
            let name = s.name().to_owned();
            let budget = self
                .per_service
                .min(self.total.saturating_sub(begin.elapsed()));
            let (tx, rx) = mpsc::channel();
            let spawned = thread::Builder::new()
                .name(format!("stop-{name}"))
                .spawn(move || {
                    s.stop();
                    let _ = tx.send(());
                });
            // A service still blocked at its deadline keeps its thread; the process exit
            // reaps it.
            if spawned.is_err() || rx.recv_timeout(budget).is_err() {
                late.push(name);
            }
        }
        if late.is_empty() {
            Ok(())
        } else {
            Err(NodeError::StopTimeout(late))
        }
    }

    /// Start, wait for a shutdown request, stop.
    pub fn run(&mut self, shutdown: &Shutdown) -> Result<(), NodeError> {
        self.start()?;
        while !shutdown.is_requested()
            && !self.internal_stop.is_requested()
            && !self.finished.is_requested()
        {
            thread::sleep(Duration::from_millis(10));
        }
        let internal = self.internal_stop.is_requested();
        self.stop()?;
        if internal {
            return Err(NodeError::DiskFull);
        }
        Ok(())
    }
}

/// Does nothing. Stands in for the p2p and rpc services that do not exist yet.
pub struct NoopService(pub &'static str);

impl Service for NoopService {
    fn name(&self) -> &str {
        self.0
    }
    fn start(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
    fn stop(&mut self) {}
}

/// Counts ticks on its own thread, for tests.
pub struct TickingService {
    ticks: Arc<AtomicU64>,
    quit: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TickingService {
    pub fn new() -> (Self, Arc<AtomicU64>) {
        let ticks = Arc::new(AtomicU64::new(0));
        let s = Self {
            ticks: ticks.clone(),
            quit: Arc::new(AtomicBool::new(false)),
            worker: None,
        };
        (s, ticks)
    }
}

impl Service for TickingService {
    fn name(&self) -> &str {
        "ticking"
    }
    fn start(&mut self) -> Result<(), ServiceError> {
        let (ticks, quit) = (self.ticks.clone(), self.quit.clone());
        let h = thread::Builder::new()
            .name("ticking".into())
            .spawn(move || {
                while !quit.load(Ordering::SeqCst) {
                    ticks.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(1));
                }
            })
            .map_err(|e| ServiceError(e.to_string()))?;
        self.worker = Some(h);
        Ok(())
    }
    fn stop(&mut self) {
        self.quit.store(true, Ordering::SeqCst);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records start/stop into a shared log; optionally blocks in stop or fails start.
    struct Fake {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        stops: Arc<AtomicU64>,
        block_stop: bool,
        fail_start: bool,
    }

    fn fake(name: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Fake {
        Fake {
            name,
            log: log.clone(),
            stops: Arc::new(AtomicU64::new(0)),
            block_stop: false,
            fail_start: false,
        }
    }

    fn push(log: &Arc<Mutex<Vec<String>>>, s: String) {
        if let Ok(mut l) = log.lock() {
            l.push(s);
        }
    }

    fn snapshot(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    impl Service for Fake {
        fn name(&self) -> &str {
            self.name
        }
        fn start(&mut self) -> Result<(), ServiceError> {
            if self.fail_start {
                return Err(ServiceError("boom".into()));
            }
            push(&self.log, format!("start {}", self.name));
            Ok(())
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            push(&self.log, format!("stop {}", self.name));
            if self.block_stop {
                thread::sleep(Duration::from_secs(60));
            }
        }
    }

    fn node() -> Node {
        Node::new(Duration::from_millis(200), Duration::from_millis(500))
    }

    #[test]
    fn an_internal_stop_ends_run_with_disk_full_after_stopping_services() {
        let log = Arc::new(Mutex::new(vec![]));
        let mut n = node();
        n.add(Box::new(fake("a", &log)));
        n.internal_stop().request();
        assert_eq!(n.run(&Shutdown::default()), Err(NodeError::DiskFull));
        assert_eq!(snapshot(&log), ["start a", "stop a"]);
        assert_eq!(NodeError::DiskFull.exit_code(), 4);
    }

    #[test]
    fn starts_in_order_stops_in_reverse() {
        let log = Arc::new(Mutex::new(vec![]));
        let mut n = node();
        for name in ["a", "b", "c"] {
            n.add(Box::new(fake(name, &log)));
        }
        assert_eq!(n.start(), Ok(()));
        assert_eq!(n.stop(), Ok(()));
        assert_eq!(
            snapshot(&log),
            ["start a", "start b", "start c", "stop c", "stop b", "stop a"]
        );
    }

    #[test]
    fn failed_start_stops_only_what_started() {
        let log = Arc::new(Mutex::new(vec![]));
        let mut n = node();
        n.add(Box::new(fake("a", &log)));
        let mut bad = fake("b", &log);
        bad.fail_start = true;
        n.add(Box::new(bad));
        n.add(Box::new(fake("c", &log)));
        let err = n.start();
        assert!(matches!(err, Err(NodeError::StartFailed { ref service, .. }) if service == "b"));
        assert_eq!(snapshot(&log), ["start a", "stop a"]);
    }

    #[test]
    fn blocked_service_cannot_hang_shutdown() {
        let log = Arc::new(Mutex::new(vec![]));
        let mut n = node();
        n.add(Box::new(fake("good", &log)));
        let mut slow = fake("slow", &log);
        slow.block_stop = true;
        n.add(Box::new(slow));
        let sd = Shutdown::default();
        sd.request();
        let begin = Instant::now();
        let r = n.run(&sd);
        assert!(
            begin.elapsed() < Duration::from_millis(900),
            "{:?}",
            begin.elapsed()
        );
        assert_eq!(r, Err(NodeError::StopTimeout(vec!["slow".into()])));
        assert_eq!(r.as_ref().err().map(NodeError::exit_code), Some(3));
        // the service after the blocked one (in stop order) still stopped
        assert!(snapshot(&log).contains(&"stop good".to_owned()));
    }

    #[test]
    fn ten_stop_requests_make_one_stop() {
        let sd = Shutdown::default();
        assert_eq!((0..10).filter(|_| sd.request()).count(), 1);
        let log = Arc::new(Mutex::new(vec![]));
        let f = fake("a", &log);
        let stops = f.stops.clone();
        let mut n = node();
        n.add(Box::new(f));
        assert_eq!(n.run(&sd), Ok(()));
        for _ in 0..10 {
            assert_eq!(n.stop(), Ok(()));
        }
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ticking_service_runs_and_stops() {
        let (t, ticks) = TickingService::new();
        let mut n = node();
        n.add(Box::new(NoopService("noop")));
        n.add(Box::new(t));
        assert_eq!(n.start(), Ok(()));
        let begin = Instant::now();
        while ticks.load(Ordering::SeqCst) == 0 && begin.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(ticks.load(Ordering::SeqCst) > 0);
        assert_eq!(n.stop(), Ok(()));
        let after = ticks.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(ticks.load(Ordering::SeqCst), after);
    }

    #[test]
    fn a_finished_service_stops_the_node_cleanly() {
        let mut node = Node::new(Duration::from_secs(1), Duration::from_secs(2));
        node.add(Box::new(NoopService("x")));
        node.finished().request();
        assert_eq!(node.run(&Shutdown::default()), Ok(()));
    }
}
