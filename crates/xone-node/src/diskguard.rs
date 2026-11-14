//! The disk watermark at work (ADR-007 section 7): samples free space of the datadir filesystem,
//! stops block import below the stop line with one alarm, resumes above the resume line, and asks
//! the node to exit when free space stays below the exit line for two samples in a row.
//! Import stops at a block boundary: the chain service reads [`ImportGate`] before each block.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
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
    fn set(&self, stop: bool) {
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

/// `(total, available)` bytes of the filesystem that holds `path`.
/// ponytail: asks `df -P -k` (POSIX) once per sample; `unsafe` is forbidden here and a statvfs
/// binding would be a new dependency. Replace with `rustix::fs::statvfs` if one is accepted.
pub fn space(path: &Path) -> io::Result<(u64, u64)> {
    let out = Command::new("df").args(["-P", "-k"]).arg(path).output()?;
    if !out.status.success() {
        return Err(io::Error::other("df failed"));
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| io::Error::other("df output not understood"))
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
            quit: None,
            worker: None,
        }
    }
}

impl Service for DiskGuard {
    fn name(&self) -> &str {
        "disk-guard"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let (total, _) = space(&self.path)
            .map_err(|e| ServiceError(format!("cannot read free space of the datadir: {e}")))?;
        let wm = DiskWatermark::from_limits(total, &self.limits).ok_or_else(|| {
            ServiceError(
                "disk watermark: exit < stop < resume does not hold (limits.disk_*)".into(),
            )
        })?;
        let (path, gate, exit) = (self.path.clone(), self.gate.clone(), self.exit.clone());
        let every = Duration::from_secs(u64::from(self.limits.disk_sample_secs.max(1)));
        let (tx, rx) = mpsc::channel::<()>();
        self.quit = Some(tx);
        self.worker = thread::Builder::new()
            .name("disk-guard".into())
            .spawn(move || {
                let mut g = Guard::new(wm);
                // a failed sample is not a reason to stop: report it once per failure run
                let mut failing = false;
                loop {
                    match space(&path) {
                        Ok((_, free)) => {
                            failing = false;
                            match g.sample(free) {
                                Event::Alarm => {
                                    gate.set(true);
                                    eprintln!("xone-node: disk_low free={free} stop_below={}; block import stopped", wm.stop_below());
                                }
                                Event::Resumed => {
                                    gate.set(false);
                                    eprintln!("xone-node: disk_ok free={free}; block import resumed");
                                }
                                Event::Exit => {
                                    gate.set(true);
                                    eprintln!("xone-node: disk_full free={free} exit_below={}; stopping the node", wm.exit_below());
                                    exit.request();
                                    return;
                                }
                                Event::None => {}
                            }
                        }
                        Err(e) if !failing => {
                            failing = true;
                            eprintln!("xone-node: disk-guard cannot sample free space: {e}");
                        }
                        Err(_) => {}
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
