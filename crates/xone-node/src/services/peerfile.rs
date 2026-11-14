//! Persistent peer table (#221): the outbound peers this node completed a handshake with, kept in
//! `<datadir>/peers.json` (or `--peers.file`) across restarts, as geth's nodes database and reth's
//! peers file do. A start dials the most recent of them after the static and trusted peers and
//! pings them for the discovery table, so a node restarted with no bootnodes finds its peers again.
//!
//! * At most [`MAX_ENTRIES`] entries; the oldest handshake is dropped first.
//! * An entry whose last handshake is older than [`EXPIRY`] (7 days) is dropped. geth expires
//!   discovery results after 24 h (`nodeDBNodeExpiration`); these are handshake-proven peers, so
//!   they are kept a week.
//! * The file is written by the 5-minute timer of the net service and at its stop, never on the
//!   import path: tmp file, fsync, rename.
//! * A file that does not parse is renamed `<file>.bad` and the node starts with an empty table.
//! * Inbound peers are not recorded: their listening port is not known.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::enode::Enode;

pub const MAX_ENTRIES: usize = 1000;
pub const EXPIRY_SECS: u64 = 7 * 24 * 3600;
/// The write interval of the net service.
pub const SAVE_EVERY_SECS: u64 = 5 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub enode: String,
    /// Unix seconds of the last completed handshake.
    pub last_handshake: u64,
}

#[derive(Serialize, Deserialize)]
struct FileJson {
    version: u32,
    peers: Vec<Entry>,
}

#[derive(Debug)]
pub struct PeerFile {
    path: PathBuf,
    /// Node id (hex) to entry.
    entries: BTreeMap<String, Entry>,
    dirty: bool,
}

impl PeerFile {
    /// Reads `path`; a missing file is an empty table, a corrupt one is moved to `<path>.bad`
    /// (the returned note says so) and the table is empty. Expired and malformed entries are
    /// dropped.
    pub fn load(path: &Path, now: u64) -> (Self, Option<String>) {
        let mut pf = Self {
            path: path.to_owned(),
            entries: BTreeMap::new(),
            dirty: false,
        };
        let text = match fs::read(path) {
            Ok(t) => t,
            Err(_) => return (pf, None),
        };
        let parsed: Result<FileJson, _> = serde_json::from_slice(&text);
        let note = match parsed {
            Ok(f) if f.version == 1 => {
                for e in f.peers {
                    pf.insert(e, now);
                }
                pf.dirty = false;
                None
            }
            _ => {
                let bad = bad_path(path);
                let _ = fs::rename(path, &bad);
                Some(format!(
                    "{}: not a peers file; moved to {} and starting with an empty table",
                    path.display(),
                    bad.display()
                ))
            }
        };
        (pf, note)
    }

    fn insert(&mut self, e: Entry, now: u64) {
        let Ok(en) = e.enode.parse::<Enode>() else {
            return;
        };
        if now.saturating_sub(e.last_handshake) > EXPIRY_SECS {
            return;
        }
        match self.entries.get(&en.id) {
            Some(old) if old.last_handshake > e.last_handshake => {}
            _ => {
                self.entries.insert(en.id, e);
                self.dirty = true;
            }
        }
        while self.entries.len() > MAX_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_handshake)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.entries.remove(&k);
                }
                None => break,
            }
        }
    }

    /// Records a completed outbound handshake with `enode` at `now`.
    pub fn record(&mut self, enode: &Enode, now: u64) {
        self.insert(
            Entry {
                enode: enode.to_string(),
                last_handshake: now,
            },
            now,
        );
    }

    /// The entries to dial, the most recent handshake first, at most `n`.
    pub fn best(&self, n: usize) -> Vec<Enode> {
        let mut v: Vec<&Entry> = self.entries.values().collect();
        v.sort_by(|a, b| b.last_handshake.cmp(&a.last_handshake));
        v.into_iter()
            .take(n)
            .filter_map(|e| e.enode.parse().ok())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Writes the table if it changed since the last write, after dropping the entries expired at
    /// `now`: tmp file, fsync, rename.
    pub fn save(&mut self, now: u64) -> std::io::Result<()> {
        let before = self.entries.len();
        self.entries
            .retain(|_, e| now.saturating_sub(e.last_handshake) <= EXPIRY_SECS);
        if !self.dirty && self.entries.len() == before {
            return Ok(());
        }
        let mut peers: Vec<Entry> = self.entries.values().cloned().collect();
        peers.sort_by(|a, b| b.last_handshake.cmp(&a.last_handshake));
        let text = serde_json::to_vec_pretty(&FileJson { version: 1, peers })
            .map_err(std::io::Error::other)?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&text)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

fn bad_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bad");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enode(b: u8, port: u16) -> Enode {
        format!("enode://{}@127.0.0.1:{port}", format!("{b:02x}").repeat(64))
            .parse()
            .unwrap_or_else(|_| Enode {
                id: String::new(),
                host: String::new(),
                port: 0,
                discport: None,
            })
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xone-peerfile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let _ = fs::create_dir_all(&d);
        d.join("peers.json")
    }

    #[test]
    fn round_trip_order_and_expiry() {
        let p = tmp("rt");
        let (mut pf, note) = PeerFile::load(&p, 1000);
        assert!(note.is_none() && pf.is_empty());
        pf.record(&enode(1, 30301), 100);
        pf.record(&enode(2, 30302), 300);
        pf.record(&enode(3, 30303), 200);
        // a newer handshake with the same node replaces the entry
        pf.record(&enode(1, 30311), 400);
        assert_eq!(pf.len(), 3);
        pf.save(400).ok();
        let (back, _) = PeerFile::load(&p, 500);
        let best: Vec<u16> = back.best(10).iter().map(|e| e.port).collect();
        assert_eq!(best, [30311, 30302, 30303]);
        assert_eq!(back.best(1).len(), 1);
        // a week later the entries of 200 and earlier are gone
        let (mut late, _) = PeerFile::load(&p, 300 + EXPIRY_SECS);
        assert_eq!(late.len(), 2);
        late.save(400 + EXPIRY_SECS + 1).ok();
        let (last, _) = PeerFile::load(&p, 400 + EXPIRY_SECS + 1);
        assert!(last.is_empty());
    }

    #[test]
    fn the_table_is_bounded() {
        let (mut pf, _) = PeerFile::load(&tmp("cap"), 0);
        for i in 0..(MAX_ENTRIES + 5) {
            let id = format!("{:0128x}", i + 1);
            let e: Enode = format!("enode://{id}@10.0.0.1:30303")
                .parse()
                .unwrap_or_else(|_| enode(0, 0));
            pf.record(&e, u64::try_from(i).unwrap_or(0));
        }
        assert_eq!(pf.len(), MAX_ENTRIES);
        // the oldest five were dropped
        assert!(!pf.entries.contains_key(&format!("{:0128x}", 1)));
        assert!(pf.entries.contains_key(&format!("{:0128x}", 6)));
    }

    #[test]
    fn a_corrupt_file_is_moved_aside() {
        let p = tmp("bad");
        fs::write(&p, b"{\"version\":1,\"peers\":[{\"enode\":").ok();
        let (pf, note) = PeerFile::load(&p, 0);
        assert!(pf.is_empty());
        assert!(note.is_some_and(|n| n.contains(".bad")));
        assert!(!p.exists());
        assert!(bad_path(&p).exists());
        // an unknown version is not trusted either
        fs::write(&p, b"{\"version\":9,\"peers\":[]}").ok();
        assert!(PeerFile::load(&p, 0).1.is_some());
    }
}
