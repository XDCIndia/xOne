//! `--synctarget` and `--exitwhensynced` (geth-XDC `cmd/utils/flags.go:203,810`, `eth/syncer/syncer.go`).
//!
//! `--synctarget <hash>` imports up to and including that block and imports nothing above it; the
//! follower then stops asking peers for headers. `--exitwhensynced` stops the node through the normal
//! shutdown path (exit 0): with a target, once the target block is held; without one, once the
//! head is less than [`RECENT_SECS`] old (the oracle's rule) and at least one sync round has run.
//! As in the oracle, a target without `--exitwhensynced` leaves the node running, idle at the
//! target. Neither flag is accepted together with `--mine`.

use crate::{
    lifecycle::{Service, ServiceError},
    services::{chain::Progress, core::Core},
    Shutdown,
};
use alloy_primitives::B256;
use std::{
    sync::{atomic::Ordering, Arc},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A head younger than this is "synced" for `--exitwhensynced` without a target
/// (`eth/syncer/syncer.go`: `time.Since(timestamp) < 10*time.Minute`).
pub const RECENT_SECS: u64 = 600;

/// Whether `target` is a block the node holds on its canonical chain.
pub fn reached(core: &Core, target: Option<B256>) -> bool {
    target.is_some_and(|t| core.hist.read().number_of(t).is_some())
}

/// What the stop rule decides from one look at the node.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Running,
    /// Stop the node, exit 0.
    Done,
}

/// The pure rule. `synced_once`: a peer confirmed our head, or a block was imported since start.
pub fn verdict(
    exit_when_synced: bool,
    target: Option<B256>,
    target_held: bool,
    head_time: u64,
    now_secs: u64,
    synced_once: bool,
) -> Verdict {
    if !exit_when_synced {
        return Verdict::Running;
    }
    let done = match target {
        Some(_) => target_held,
        None => synced_once && now_secs.saturating_sub(head_time) < RECENT_SECS,
    };
    if done {
        Verdict::Done
    } else {
        Verdict::Running
    }
}

/// Watches the head and asks the node to stop when [`verdict`] says so.
pub struct SyncStop {
    core: Arc<Core>,
    progress: Arc<Progress>,
    target: Option<B256>,
    exit_when_synced: bool,
    finish: Shutdown,
    quit: Shutdown,
    task: Option<JoinHandle<()>>,
}

impl SyncStop {
    pub fn new(
        core: Arc<Core>,
        progress: Arc<Progress>,
        target: Option<B256>,
        exit_when_synced: bool,
        finish: Shutdown,
    ) -> Self {
        Self {
            core,
            progress,
            target,
            exit_when_synced,
            finish,
            quit: Shutdown::default(),
            task: None,
        }
    }
}

impl Service for SyncStop {
    fn name(&self) -> &str {
        "syncstop"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let (core, progress, target, ews, finish, quit) = (
            self.core.clone(),
            self.progress.clone(),
            self.target,
            self.exit_when_synced,
            self.finish.clone(),
            self.quit.clone(),
        );
        let start = progress.start.load(Ordering::Relaxed);
        let task = thread::Builder::new()
            .name("syncstop".into())
            .spawn(move || {
                let mut told = false;
                while !quit.is_requested() {
                    let held = reached(&core, target);
                    if held && !told {
                        told = true;
                        let (n, h) = core.head();
                        eprintln!("sync: target reached, number {n} hash {h}");
                    }
                    let (head, head_time) = {
                        let g = core.hist.read();
                        (g.tip_number(), g.tip().header.timestamp)
                    };
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let synced_once = head > start || progress.confirmed.load(Ordering::Relaxed);
                    if verdict(ews, target, held, head_time, now, synced_once) == Verdict::Done {
                        eprintln!("sync: synced, stopping the node (--exitwhensynced)");
                        finish.request();
                        return;
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            })
            .map_err(|e| ServiceError(e.to_string()))?;
        self.task = Some(task);
        Ok(())
    }

    fn stop(&mut self) {
        self.quit.request();
        if let Some(t) = self.task.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_rules_follow_the_oracle() {
        let t = Some(B256::repeat_byte(1));
        // nothing without --exitwhensynced, even at the target
        assert_eq!(verdict(false, t, true, 0, 0, true), Verdict::Running);
        // with a target: only once the target is held
        assert_eq!(verdict(true, t, false, 100, 100, true), Verdict::Running);
        assert_eq!(verdict(true, t, true, 0, 10_000, false), Verdict::Done);
        // without one: a head younger than 10 minutes, after a sync round
        assert_eq!(
            verdict(true, None, false, 1_000, 1_599, true),
            Verdict::Done
        );
        assert_eq!(
            verdict(true, None, false, 1_000, 1_600, true),
            Verdict::Running
        );
        assert_eq!(
            verdict(true, None, false, 1_000, 1_001, false),
            Verdict::Running
        );
    }
}
