//! The disk watermark at work (ADR-007 section 7): samples free space of the datadir filesystem,
//! stops block import below the stop line with one alarm, resumes above the resume line, and asks
//! the node to exit when free space stays below the exit line for two samples in a row.
//! Import stops at a block boundary: the chain service reads [`ImportGate`] before each block, and
//! the producer before it seals one. The first sample is taken in `start`, before any writer runs
//! ([`admit`] does the same check before snap sync). `df` has [`DF_TIMEOUT`]; after
//! [`STALE_SAMPLES`] failed samples in a row the gate closes until a sample succeeds again.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    config::Limits,
    lifecycle::{Service, ServiceError, Shutdown},
    Action, DiskWatermark,
};

/// Read by the chain service; written by the guard. Open by default.
#[derive(Clone, Default)]
pub struct ImportGate(Arc<AtomicBool>);

impl ImportGate {
    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub(crate) fn set(&self, stop: bool) {
        self.0.store(stop, Ordering::Relaxed);
    }
}

/// What one sample changed. The caller prints the alarm; the pure step only decides.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    /// Transition into the stop state: raise the alarm once.
    Alarm,
    Resumed,
    /// Two samples in a row below the exit line.
    Exit,
}

/// Pure state of the guard.
pub struct Guard {
    wm: DiskWatermark,
    state: Action,
    below_exit: u32,
}

impl Guard {
    pub fn new(wm: DiskWatermark) -> Self {
        Self {
            wm,
            state: Action::Continue,
            below_exit: 0,
        }
    }

    pub fn state(&self) -> Action {
        self.state
    }

    pub fn sample(&mut self, free: u64) -> Event {
        let mut next = self.wm.decide(free, self.state);
        // `Exit` is terminal in `decide`, so the two-sample rule is counted here: one low reading
        // (a snapshot being written, a log burst) stops import but must not stop the node.
        if next == Action::Exit {
            self.below_exit += 1;
            if self.below_exit >= 2 {
                self.state = Action::Exit;
                return Event::Exit;
            }
            next = Action::StopImportAlarm;
        } else {
            self.below_exit = 0;
        }
        let was = std::mem::replace(&mut self.state, next);
        match (was, next) {
            (Action::Continue, Action::StopImportAlarm) => Event::Alarm,
            (Action::StopImportAlarm, Action::Continue) => Event::Resumed,
            _ => Event::None,
        }
    }
}

/// How long one `df` may run; a hung network mount must not freeze the guard.
pub const DF_TIMEOUT: Duration = Duration::from_secs(10);
/// Failed samples in a row after which the gate closes: no reading is not "enough space".
pub const STALE_SAMPLES: u32 = 3;

/// `(total, available)` bytes of the filesystem that holds `path`.
/// ponytail: asks `df -P -k` (POSIX) once per sample; `unsafe` is forbidden here and a statvfs
/// binding would be a new dependency. Replace with `rustix::fs::statvfs` if one is accepted.
pub fn space(path: &Path) -> io::Result<(u64, u64)> {
    let mut child = Command::new("df")
        .args(["-P", "-k"])
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let begin = Instant::now();
    // the output is two short lines: it fits the pipe, so waiting before reading cannot block df
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if begin.elapsed() >= DF_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "df timed out"));
        }
        thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(io::Error::other("df failed"));
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| io::Error::other("df output not understood"))
}

/// The admission check before a writer that runs outside the services (snap sync): refused when
/// free space is below the stop line, or cannot be read.
pub fn admit(path: &Path, limits: &Limits) -> Result<(), String> {
    let (total, free) =
        space(path).map_err(|e| format!("cannot read free space of the datadir: {e}"))?;
    let wm = DiskWatermark::from_limits(total, limits)
        .ok_or("disk watermark: exit < stop < resume does not hold (limits.disk_*)")?;
    if free < wm.stop_below() {
        return Err(format!(
            "disk_low free={free} stop_below={}: not starting to write",
            wm.stop_below()
        ));
    }
    Ok(())
}

/// The gate and the log for one sample's event. `true`: the node must exit.
fn apply(ev: Event, free: u64, wm: &DiskWatermark, gate: &ImportGate) -> bool {
    match ev {
        Event::Alarm => {
            gate.set(true);
            eprintln!(
                "xone-node: disk_low free={free} stop_below={}; block import stopped",
                wm.stop_below()
            );
        }
        Event::Resumed => {
            gate.set(false);
            eprintln!("xone-node: disk_ok free={free}; block import resumed");
        }
        Event::Exit => {
            gate.set(true);
            eprintln!(
                "xone-node: disk_full free={free} exit_below={}; stopping the node",
                wm.exit_below()
            );
            return true;
        }
        Event::None => {}
    }
    false
}

/// Failed-sample bookkeeping: the gate closes at [`STALE_SAMPLES`] failures in a row, and opens
/// again at the next good sample when the watermark state allows it.
#[derive(Default)]
struct Staleness {
    fails: u32,
    closed: bool,
}

impl Staleness {
    /// A failed sample. `true` when this one closes the gate.
    fn failed(&mut self) -> bool {
        self.fails = self.fails.saturating_add(1);
        let close = self.fails >= STALE_SAMPLES && !self.closed;
        self.closed |= close;
        close
    }

    /// A good sample. `true` when the gate was closed for staleness (the caller reopens it if the
    /// watermark allows).
    fn ok(&mut self) -> bool {
        self.fails = 0;
        std::mem::take(&mut self.closed)
    }
}

fn parse_df(text: &str) -> Option<(u64, u64)> {
    let f: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let kib = |i: usize| f.get(i)?.parse::<u64>().ok()?.checked_mul(1024);
    Some((kib(1)?, kib(3)?))
}

pub struct DiskGuard {
    path: PathBuf,
    limits: Limits,
    gate: ImportGate,
    exit: Shutdown,
    exited: Arc<AtomicBool>,
    quit: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl DiskGuard {
    /// `exit` is asked to stop the node when free space is exhausted.
    pub fn new(path: PathBuf, limits: Limits, gate: ImportGate, exit: Shutdown) -> Self {
        Self {
            path,
            limits,
            gate,
            exit,
            exited: Arc::default(),
            quit: None,
            worker: None,
        }
    }

    /// Whether the guard asked the node to exit for lack of space.
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }
}

impl Service for DiskGuard {
    fn name(&self) -> &str {
        "disk-guard"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let (total, free) = space(&self.path)
            .map_err(|e| ServiceError(format!("cannot read free space of the datadir: {e}")))?;
        let wm = DiskWatermark::from_limits(total, &self.limits).ok_or_else(|| {
            ServiceError(
                "disk watermark: exit < stop < resume does not hold (limits.disk_*)".into(),
            )
        })?;
        // the first sample decides the gate before any writer starts (the guard is the first service)
        let mut g = Guard::new(wm);
        if apply(g.sample(free), free, &wm, &self.gate) {
            self.exited.store(true, Ordering::Relaxed);
            self.exit.request();
        }
        let (path, gate, exit, exited) = (
            self.path.clone(),
            self.gate.clone(),
            self.exit.clone(),
            self.exited.clone(),
        );
        let every = Duration::from_secs(u64::from(self.limits.disk_sample_secs.max(1)));
        let (tx, rx) = mpsc::channel::<()>();
        self.quit = Some(tx);
        self.worker = thread::Builder::new()
            .name("disk-guard".into())
            .spawn(move || {
                if exited.load(Ordering::Relaxed) {
                    return;
                }
                // a failed sample is not a reason to stop the node, but enough of them close the gate
                let mut stale = Staleness::default();
                loop {
                    match space(&path) {
                        Ok((_, free)) => {
                            if stale.ok() && g.state() == Action::Continue {
                                gate.set(false);
                                eprintln!("xone-node: disk-guard sampling again, free={free}; block import resumed");
                            }
                            if apply(g.sample(free), free, &wm, &gate) {
                                exited.store(true, Ordering::Relaxed);
                                exit.request();
                                return;
                            }
                        }
                        Err(e) => {
                            if stale.fails == 0 {
                                eprintln!("xone-node: disk-guard cannot sample free space: {e}");
                            }
                            if stale.failed() {
                                gate.set(true);
                                eprintln!("xone-node: disk-guard has no valid sample for {STALE_SAMPLES} tries; block import stopped until one succeeds");
                            }
                        }
                    }
                    if rx.recv_timeout(every) != Err(mpsc::RecvTimeoutError::Timeout) {
                        return;
                    }
                }
            })
            .map(Some)
            .map_err(|e| ServiceError(e.to_string()))?;
        Ok(())
    }

    fn stop(&mut self) {
        self.quit.take();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> Guard {
        Guard::new(DiskWatermark::new(20, 100, 150).unwrap())
    }

    #[test]
    fn one_alarm_then_resume_only_above_the_resume_line() {
        let mut g = g();
        let ev: Vec<Event> = [500, 99, 98, 120, 149, 151].map(|f| g.sample(f)).into();
        assert_eq!(
            ev,
            [
                Event::None,
                Event::Alarm,
                Event::None,
                Event::None,
                Event::None,
                Event::Resumed
            ]
        );
    }

    #[test]
    fn exit_needs_two_low_samples_and_a_single_dip_does_not_count() {
        let mut g = g();
        assert_eq!(g.sample(10), Event::Alarm); // first low sample: import stops, no exit yet
        assert_eq!(g.state(), Action::StopImportAlarm);
        assert_eq!(g.sample(90), Event::None); // dip over, still stopped (below resume)
        assert_eq!(g.sample(10), Event::None); // counted from zero again: stopped, not exiting
        assert_eq!(g.sample(10), Event::Exit);
    }

    #[test]
    fn two_low_samples_in_a_row_exit() {
        let mut g = g();
        assert_eq!(g.sample(10), Event::Alarm);
        assert_eq!(g.sample(10), Event::Exit);
        assert_eq!(g.state(), Action::Exit);
    }

    #[test]
    fn stale_sampler_closes_then_reopens() {
        let mut s = Staleness::default();
        assert!(!s.failed());
        assert!(!s.failed());
        assert!(s.failed()); // the third failure in a row closes the gate
        assert!(!s.failed()); // once
        assert!(s.ok()); // a good sample reports the staleness close
        assert!(!s.ok());
        assert!(!s.failed()); // counted from zero again
    }

    #[test]
    fn first_sample_closes_the_gate_before_writers_start() {
        let wm = DiskWatermark::new(20, 100, 150).unwrap();
        let gate = ImportGate::default();
        let mut g = Guard::new(wm);
        assert!(!apply(g.sample(50), 50, &wm, &gate));
        assert!(gate.stopped());
        let gate = ImportGate::default();
        let mut g = Guard::new(wm);
        assert!(!apply(g.sample(500), 500, &wm, &gate));
        assert!(!gate.stopped());
    }

    #[test]
    fn admit_reads_this_filesystem() {
        assert!(admit(Path::new("."), &Limits::default()).is_ok());
        // a stop line above any disk: refused
        let l = Limits {
            disk_stop_pct: 99,
            disk_resume_pct: 100,
            disk_stop_min_bytes: u64::MAX / 4,
            disk_resume_min_bytes: u64::MAX / 2,
            ..Limits::default()
        };
        assert!(admit(Path::new("."), &l).is_err());
    }

    #[test]
    fn parses_df() {
        let t = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 1000 400 600 40% /\n";
        assert_eq!(parse_df(t), Some((1000 * 1024, 600 * 1024)));
        assert_eq!(parse_df("garbage"), None);
    }

    #[test]
    fn real_df_reads_this_filesystem() {
        let (total, free) = space(Path::new(".")).unwrap();
        assert!(total > 0 && free <= total);
    }
}
