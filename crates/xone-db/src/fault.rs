//! The in-memory file system of `tests/fault/PLAN.md` section 2. Tests only: this module exists
//! under the `fault-injection` feature and must not be in a release artifact.
//!
//! Two layers: `durable` is what a returned `fsync` covers, `pending` is the ordered list of
//! unsynced writes, truncations, creates, renames and removes. The live view is both layers
//! together. [`MemDir::crash`] builds the disk a restart finds: `durable` plus the subset of
//! `pending` that a [`Pick`] chooses (F1 to F5). Calls can also be made to fail (F6 to F8).
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::segments::{Probe, SegDir, SegFile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Errno {
    Eio,
    Enospc,
}

impl Errno {
    fn err(self) -> io::Error {
        io::Error::from_raw_os_error(match self {
            Errno::Eio => 5,
            Errno::Enospc => 28,
        })
    }
}

/// Which unsynced operations survive a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// F1: `kill -9`. Everything passed to `write` is in the page cache and survives.
    Everything,
    /// F2: power loss, fsync honoured. Nothing unsynced survives.
    Nothing,
    /// One unsynced operation survives alone.
    Only(usize),
    /// Every unsynced operation survives except this one.
    Except(usize),
    /// F5: the first `k` unsynced operations reached the device, the rest never did.
    Prefix(usize),
    /// F3: the first `keep` operations survive whole; the next one, if a write, is cut at a
    /// seeded multiple of `grid` bytes.
    Torn { keep: usize, grid: usize, seed: u64 },
    /// F4: a seeded subset survives, in a seeded order.
    Random { seed: u64 },
}

#[derive(Clone, Debug)]
enum Op {
    Write { ino: u64, off: u64, data: Vec<u8> },
    SetLen { ino: u64, len: u64 },
    Create { name: String, ino: u64 },
    Rename { from: String, to: String },
    Remove { name: String },
}

#[derive(Clone, Default)]
struct Image {
    names: BTreeMap<String, u64>,
    files: BTreeMap<u64, Vec<u8>>,
}

fn parent(name: &str) -> &str {
    name.rsplit_once('/').map_or(".", |(d, _)| d)
}

impl Image {
    fn apply(&mut self, op: &Op) {
        match op {
            Op::Write { ino, off, data } => {
                let f = self.files.entry(*ino).or_default();
                let end = *off as usize + data.len();
                if f.len() < end {
                    f.resize(end, 0);
                }
                f[*off as usize..end].copy_from_slice(data);
            }
            Op::SetLen { ino, len } => self.files.entry(*ino).or_default().resize(*len as usize, 0),
            Op::Create { name, ino } => {
                self.files.entry(*ino).or_default();
                self.names.insert(name.clone(), *ino);
            }
            Op::Rename { from, to } => {
                if let Some(i) = self.names.remove(from) {
                    self.names.insert(to.clone(), i);
                }
            }
            Op::Remove { name } => {
                self.names.remove(name);
            }
        }
    }
}

#[derive(Default)]
struct Faults {
    sync: Option<(u64, Errno)>,
    write: Option<(u64, Errno, bool)>,
    read: Option<u64>,
}

struct Fs {
    durable: Image,
    view: Image,
    pending: Vec<Op>,
    next_ino: u64,
    boot: String,
    faults: Faults,
    syncs: u64,
    writes: u64,
    reads: u64,
    armed: Option<(String, Probe, usize)>,
    visited: Vec<&'static str>,
}

/// A cloneable handle: the store owns one, the test keeps another to crash the disk.
#[derive(Clone)]
pub struct MemDir(Arc<Mutex<Fs>>);

fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Default for MemDir {
    fn default() -> Self {
        Self::new()
    }
}

impl MemDir {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Fs {
            durable: Image::default(),
            view: Image::default(),
            pending: Vec::new(),
            next_ino: 1,
            boot: "boot-1".into(),
            faults: Faults::default(),
            syncs: 0,
            writes: 0,
            reads: 0,
            armed: None,
            visited: Vec::new(),
        })))
    }

    fn fs(&self) -> MutexGuard<'_, Fs> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Unsynced operations: the size of the space [`Pick`] chooses from.
    pub fn pending(&self) -> usize {
        self.fs().pending.len()
    }

    /// `fsync` calls so far, files and directories together.
    pub fn syncs(&self) -> u64 {
        self.fs().syncs
    }

    pub fn set_boot(&self, id: &str) {
        self.fs().boot = id.into();
    }

    /// F6: the `nth` (1-based, counted from now) `fsync` call fails once.
    pub fn fail_sync(&self, nth: u64, e: Errno) {
        let mut f = self.fs();
        f.faults.sync = Some((f.syncs + nth, e));
    }

    /// F7 and F8: the `nth` write, truncate or create fails; with `sticky`, every later one too.
    pub fn fail_write(&self, nth: u64, e: Errno, sticky: bool) {
        let mut f = self.fs();
        f.faults.write = Some((f.writes + nth, e, sticky));
    }

    /// F8: the `nth` read fails once.
    pub fn fail_read(&self, nth: u64) {
        let mut f = self.fs();
        f.faults.read = Some(f.reads + nth);
    }

    pub fn clear_faults(&self) {
        self.fs().faults = Faults::default();
    }

    /// The disk a restart finds. Nothing is pending on it and no fault is armed.
    pub fn crash(&self, pick: Pick) -> MemDir {
        let f = self.fs();
        let n = f.pending.len();
        let mut chosen: Vec<Op> = Vec::new();
        match pick {
            Pick::Everything => chosen = f.pending.clone(),
            Pick::Nothing => {}
            Pick::Only(i) => chosen.extend(f.pending.get(i).cloned()),
            Pick::Except(i) => {
                chosen = (0..n)
                    .filter(|k| *k != i)
                    .map(|k| f.pending[k].clone())
                    .collect();
            }
            Pick::Prefix(k) => chosen = f.pending[..k.min(n)].to_vec(),
            Pick::Torn { keep, grid, seed } => {
                chosen = f.pending[..keep.min(n)].to_vec();
                if let Some(Op::Write { ino, off, data }) = f.pending.get(keep) {
                    let mut s = seed;
                    let cut = (splitmix(&mut s) as usize % (data.len() / grid.max(1) + 1)) * grid;
                    if cut > 0 {
                        chosen.push(Op::Write {
                            ino: *ino,
                            off: *off,
                            data: data[..cut.min(data.len())].to_vec(),
                        });
                    }
                }
            }
            Pick::Random { mut seed } => {
                let mut kept: Vec<Op> = f
                    .pending
                    .iter()
                    .filter(|_| splitmix(&mut seed) & 1 == 1)
                    .cloned()
                    .collect();
                for i in (1..kept.len()).rev() {
                    kept.swap(i, splitmix(&mut seed) as usize % (i + 1));
                }
                chosen = kept;
            }
        }
        let mut img = f.durable.clone();
        for op in &chosen {
            img.apply(op);
        }
        MemDir(Arc::new(Mutex::new(Fs {
            durable: img.clone(),
            view: img,
            pending: Vec::new(),
            next_ino: f.next_ino,
            boot: f.boot.clone(),
            faults: Faults::default(),
            syncs: 0,
            writes: 0,
            reads: 0,
            armed: None,
            visited: Vec::new(),
        })))
    }

    /// The next time the store reaches kill point `point` (after `skip` earlier visits), `probe` fires.
    pub fn arm(&self, point: &str, probe: Probe, skip: usize) {
        self.fs().armed = Some((point.into(), probe, skip));
    }

    /// Kill points the store has passed, in order.
    pub fn visited(&self) -> Vec<&'static str> {
        self.fs().visited.clone()
    }

    /// A hash of the live image (names and bytes): crash images that are equal need one run.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let f = self.fs();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (n, i) in &f.view.names {
            (n, f.view.files.get(i)).hash(&mut h);
        }
        h.finish()
    }

    /// The live bytes of a file, for assertions.
    pub fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        let f = self.fs();
        f.view
            .names
            .get(name)
            .and_then(|i| f.view.files.get(i))
            .cloned()
    }

    pub fn exists(&self, name: &str) -> bool {
        self.fs().view.names.contains_key(name)
    }

    /// Direct damage to the live and durable bytes (a flipped bit, a cut file): not a crash.
    pub fn damage(&self, name: &str, f: impl Fn(&mut Vec<u8>)) {
        let mut guard = self.fs();
        let fs = &mut *guard;
        if let Some(i) = fs.view.names.get(name).copied() {
            for img in [&mut fs.view, &mut fs.durable] {
                if let Some(b) = img.files.get_mut(&i) {
                    f(b);
                }
            }
        }
    }
}

impl Fs {
    fn log(&mut self, op: Op) {
        self.view.apply(&op);
        self.pending.push(op);
    }

    fn write_gate(&mut self) -> io::Result<()> {
        self.writes += 1;
        match self.faults.write {
            Some((n, e, sticky)) if self.writes == n || (sticky && self.writes > n) => {
                if !sticky {
                    self.faults.write = None;
                }
                Err(e.err())
            }
            _ => Ok(()),
        }
    }

    fn sync_gate(&mut self) -> io::Result<()> {
        self.syncs += 1;
        match self.faults.sync {
            Some((n, e)) if self.syncs == n => {
                self.faults.sync = None;
                Err(e.err())
            }
            _ => Ok(()),
        }
    }
}

pub struct MemFile {
    fs: Arc<Mutex<Fs>>,
    ino: u64,
}

impl MemFile {
    fn fs(&self) -> MutexGuard<'_, Fs> {
        self.fs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SegFile for MemFile {
    fn write_at(&mut self, off: u64, data: &[u8]) -> io::Result<()> {
        let mut f = self.fs();
        f.write_gate()?;
        f.log(Op::Write {
            ino: self.ino,
            off,
            data: data.to_vec(),
        });
        Ok(())
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut f = self.fs();
        f.reads += 1;
        if f.faults.read == Some(f.reads) {
            f.faults.read = None;
            return Err(Errno::Eio.err());
        }
        let b = f.view.files.get(&self.ino).map_or(&[][..], |v| &v[..]);
        let (s, e) = (off as usize, off as usize + buf.len());
        if e > b.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.copy_from_slice(&b[s..e]);
        Ok(())
    }
    fn size(&self) -> io::Result<u64> {
        Ok(self
            .fs()
            .view
            .files
            .get(&self.ino)
            .map_or(0, |v| v.len() as u64))
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        let mut f = self.fs();
        f.write_gate()?;
        f.log(Op::SetLen { ino: self.ino, len });
        Ok(())
    }
    fn sync_all(&mut self) -> io::Result<()> {
        let mut f = self.fs();
        f.sync_gate()?;
        let ino = self.ino;
        let f = &mut *f;
        let (mine, rest): (Vec<Op>, Vec<Op>) =
            std::mem::take(&mut f.pending).into_iter().partition(
                |o| matches!(o, Op::Write { ino: i, .. } | Op::SetLen { ino: i, .. } if *i == ino),
            );
        for o in &mine {
            f.durable.apply(o);
        }
        f.pending = rest;
        Ok(())
    }
}

impl SegDir for MemDir {
    type File = MemFile;
    fn create(&self, name: &str) -> io::Result<MemFile> {
        let mut f = self.fs();
        f.write_gate()?;
        let ino = match f.view.names.get(name) {
            Some(i) => {
                let i = *i;
                f.log(Op::SetLen { ino: i, len: 0 });
                i
            }
            None => {
                let i = f.next_ino;
                f.next_ino += 1;
                f.log(Op::Create {
                    name: name.into(),
                    ino: i,
                });
                i
            }
        };
        Ok(MemFile {
            fs: self.0.clone(),
            ino,
        })
    }
    fn open(&self, name: &str) -> io::Result<Option<MemFile>> {
        Ok(self.fs().view.names.get(name).map(|i| MemFile {
            fs: self.0.clone(),
            ino: *i,
        }))
    }
    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        Ok(self
            .fs()
            .view
            .names
            .keys()
            .filter(|n| parent(n) == dir)
            .map(|n| n.rsplit('/').next().unwrap_or(n).to_owned())
            .collect())
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let mut f = self.fs();
        f.write_gate()?;
        if !f.view.names.contains_key(from) {
            return Err(io::ErrorKind::NotFound.into());
        }
        f.log(Op::Rename {
            from: from.into(),
            to: to.into(),
        });
        Ok(())
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        let mut f = self.fs();
        f.write_gate()?;
        if f.view.names.contains_key(name) {
            f.log(Op::Remove { name: name.into() });
        }
        Ok(())
    }
    fn sync(&self, dir: &str) -> io::Result<()> {
        let mut f = self.fs();
        f.sync_gate()?;
        let f = &mut *f;
        let is_dir_op = |o: &Op| match o {
            Op::Create { name, .. } | Op::Remove { name } => parent(name) == dir,
            Op::Rename { to, .. } => parent(to) == dir,
            _ => false,
        };
        let (mine, rest): (Vec<Op>, Vec<Op>) = std::mem::take(&mut f.pending)
            .into_iter()
            .partition(is_dir_op);
        for o in &mine {
            f.durable.apply(o);
        }
        f.pending = rest;
        Ok(())
    }
    fn boot_id(&self) -> String {
        self.fs().boot.clone()
    }
    fn probe(&self, point: &'static str) -> Option<Probe> {
        let mut f = self.fs();
        f.visited.push(point);
        let a = f.armed.as_mut().filter(|a| a.0 == point)?;
        if a.2 > 0 {
            a.2 -= 1;
            return None;
        }
        let p = a.1;
        f.armed = None;
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_images() {
        let d = MemDir::new();
        let mut f = d.create("a/x").unwrap();
        f.write_at(0, b"hello").unwrap();
        f.sync_all().unwrap(); // data durable, name not
        assert_eq!(
            d.crash(Pick::Nothing).open("a/x").unwrap().map(|_| ()),
            None
        );
        d.sync("a").unwrap();
        f.write_at(5, b"world").unwrap();
        let b = |p| d.crash(p).bytes("a/x").unwrap();
        assert_eq!(b(Pick::Nothing), b"hello");
        assert_eq!(b(Pick::Everything), b"helloworld");
        assert_eq!(
            b(Pick::Torn {
                keep: 0,
                grid: 2,
                seed: 3
            })
            .len()
                % 2,
            1
        );
        assert_eq!(d.pending(), 1);
        // a rename needs its directory synced
        d.rename("a/x", "a/y").unwrap();
        assert!(d.crash(Pick::Nothing).exists("a/x"));
        assert!(d.crash(Pick::Everything).exists("a/y"));
        d.sync("a").unwrap();
        assert!(d.crash(Pick::Nothing).exists("a/y"));
    }

    #[test]
    fn faults_fire_once_at_the_nth_call() {
        let d = MemDir::new();
        let mut f = d.create("a").unwrap();
        d.fail_sync(2, Errno::Eio);
        assert!(f.sync_all().is_ok());
        assert_eq!(f.sync_all().unwrap_err().raw_os_error(), Some(5));
        assert!(f.sync_all().is_ok());
        d.fail_write(1, Errno::Enospc, true);
        assert!(f.write_at(0, b"x").is_err());
        assert!(f.write_at(0, b"x").is_err());
        d.clear_faults();
        assert!(f.write_at(0, b"x").is_ok());
    }
}
