//! Test support for the ADR-005 fault harness: stores over the in-memory disk with a real MDBX
//! environment in a scratch directory, and the helpers every test shares.
#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use xone_db::{fault::MemDir, segments::Probe};
use xone_primitives::Header;
use xone_state::MemFlat;
use xone_sync::{
    mdbx::{Config, MdbxStore},
    store::KillPoint,
    testkit::Fault,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Self {
        // tmpfs when there is one: the matrix creates thousands of environments
        let root = if Path::new("/dev/shm").is_dir() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };
        let d = root.join(format!(
            "xone-fault-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        Self(d)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub type MemStoreOnDisk = MdbxStore<MemDir>;

pub fn cfg(retention: u64, per_file: u64) -> Config {
    Config {
        retention,
        per_file,
        group_commit: None,
        // XONE_DISK_STATE=1 runs every suite over the disk flat state (`--state.disk`)
        disk_state: std::env::var_os("XONE_DISK_STATE").is_some(),
        geometry: Default::default(),
    }
}

/// A new datadir: real MDBX in `scratch`, segments on the in-memory disk, the anchor committed.
pub fn anchored(
    scratch: &Scratch,
    dir: MemDir,
    header: Header,
    flat: MemFlat,
    cfg: Config,
) -> MemStoreOnDisk {
    let mut s = MdbxStore::open(&scratch.path().join("chain.mdbx"), dir, cfg)
        .unwrap_or_else(|e| panic!("open: {e}"));
    s.init_anchor(header, flat)
        .unwrap_or_else(|e| panic!("anchor: {e}"));
    s
}

/// A copy of an MDBX directory (the store that held it is dropped): one image per crash case.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap_or_else(|e| panic!("{e}"));
    for e in std::fs::read_dir(from)
        .unwrap_or_else(|e| panic!("{e}"))
        .flatten()
    {
        if e.file_name() != "mdbx.lck" {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap_or_else(|e| panic!("{e}"));
        }
    }
}

/// Arming and the kill-point log of a store on the in-memory disk.
pub trait Arm {
    /// The next time `point` is reached (after `skip` earlier visits), `fault` happens.
    fn arm(&mut self, point: KillPoint, fault: Fault, skip: usize);
    /// The kill points passed so far.
    fn log(&self) -> Vec<KillPoint>;
}

impl Arm for MdbxStore<MemDir> {
    fn arm(&mut self, point: KillPoint, fault: Fault, skip: usize) {
        let probe = match fault {
            Fault::Kill | Fault::Power => Probe::Crash,
            Fault::Io => Probe::Io,
            Fault::IoCommitted => Probe::IoCommitted,
        };
        self.segs().dir.arm(point.name(), probe, skip);
    }
    fn log(&self) -> Vec<KillPoint> {
        let seen = self.segs().dir.visited();
        KillPoint::ALL
            .iter()
            .chain(&KillPoint::PRUNE)
            .chain(&KillPoint::REPAIR)
            .filter(|k| seen.contains(&k.name()))
            .copied()
            .collect()
    }
}
