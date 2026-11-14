//! RPC traffic recorder (issue #197, first slice): `--rpc.record <dir>` writes each request the
//! node dispatches as one JSON line (`t` Unix ms, `transport`, `method`, `params`) to
//! `<dir>/rpc-record.jsonl`, rotated by size to `.1` .. `.N`. Off by default.
//!
//! Never written: HTTP headers (the recorder sits after the transport, so an `Authorization` or JWT
//! never reaches it) and the methods of [`denied`] (`admin_*`, `personal_*`, `perm_*` and the
//! signing calls). A full queue drops the line and counts it; a request never waits on the disk.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        mpsc::{sync_channel, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

/// Lines waiting for the writer thread.
const QUEUE: usize = 4_096;
/// The file name in the record directory.
pub const FILE: &str = "rpc-record.jsonl";

/// Methods that are never recorded: node administration and anything that signs or carries a
/// password or key material.
pub fn denied(method: &str) -> bool {
    method.starts_with("admin_")
        || method.starts_with("personal_")
        || method.starts_with("perm_")
        || method.starts_with("clique_")
        || matches!(
            method,
            "eth_sign"
                | "eth_signTransaction"
                | "eth_sendTransaction"
                | "eth_signTypedData"
                | "eth_signTypedData_v3"
                | "eth_signTypedData_v4"
        )
}

pub struct Recorder {
    tx: SyncSender<String>,
    dropped: AtomicU64,
    written: std::sync::Arc<AtomicU64>,
}

impl Recorder {
    /// Opens `<dir>/rpc-record.jsonl` for append; rotates at `max_bytes`, keeps `files` files.
    pub fn open(dir: &Path, max_bytes: u64, files: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        let f = open_append(&path)?;
        let size = f.metadata()?.len();
        let (tx, rx) = sync_channel(QUEUE);
        let written = std::sync::Arc::new(AtomicU64::new(0));
        let w = written.clone();
        thread::Builder::new()
            .name("rpc-record".into())
            .spawn(move || writer(rx, path, f, size, max_bytes.max(1), files.max(1), &w))?;
        Ok(Self {
            tx,
            dropped: AtomicU64::new(0),
            written,
        })
    }

    /// Queues one request; never blocks.
    pub fn record(&self, transport: &str, method: &str, params: Option<&Value>) {
        if denied(method) {
            return;
        }
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let line = json!({
            "t": t, "transport": transport, "method": method,
            "params": params.cloned().unwrap_or(Value::Null),
        })
        .to_string();
        match self.tx.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Relaxed);
            }
        }
    }

    /// Lines dropped because the writer was behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    /// Lines the writer has written and flushed.
    pub fn written(&self) -> u64 {
        self.written.load(Relaxed)
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

fn rotated(path: &Path, i: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{i}"));
    PathBuf::from(s)
}

fn writer(
    rx: Receiver<String>,
    path: PathBuf,
    f: File,
    mut size: u64,
    max_bytes: u64,
    files: usize,
    written: &AtomicU64,
) {
    let mut out = BufWriter::new(f);
    while let Ok(first) = rx.recv() {
        let mut n = 0u64;
        // write what is queued, then flush once
        for line in std::iter::once(first).chain(rx.try_iter()) {
            let len = u64::try_from(line.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            if size > 0 && size.saturating_add(len) > max_bytes {
                let _ = out.flush();
                for i in (1..files).rev() {
                    let from = if i == 1 {
                        path.clone()
                    } else {
                        rotated(&path, i - 1)
                    };
                    let _ = fs::rename(&from, rotated(&path, i));
                }
                if files == 1 {
                    let _ = fs::remove_file(&path);
                }
                match open_append(&path) {
                    Ok(nf) => out = BufWriter::new(nf),
                    // the recorder stops; later lines count as dropped
                    Err(_) => return,
                }
                size = 0;
            }
            if writeln!(out, "{line}").is_ok() {
                size = size.saturating_add(len);
                n += 1;
            }
        }
        let _ = out.flush();
        written.fetch_add(n, Relaxed);
    }
}
