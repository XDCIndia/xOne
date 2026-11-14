//! Static-file segments (ADR-005 sections 5, 6.1, 6.4 and 7): one append-only chunk file per
//! segment and 100,000 blocks, CRC-32C frames, the visible-length publication model and the
//! start-up scanner that feeds [`crate::reconcile`] with real tails.
//!
//! All file access goes through [`SegDir`] and [`SegFile`], so the same writer runs on the real
//! file system ([`OsDir`]) and on the in-memory crash model of `crate::fault` (feature
//! `fault-injection`, tests only). A file here carries no authority: bytes beyond the committed
//! length of the manifest are staged, never read.
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_primitives::{keccak256, Keccak256, B256};

use crate::{
    manifest::{Manifest, Segment, SegmentEntry, FORMAT_VERSION},
    recover::{ManifestState, SegmentTail},
    Cause,
};

pub const BLOCKS_PER_FILE: u64 = 100_000;
pub const HEADER_LEN: u64 = 20;
const FRAME_HEAD: usize = 12;
const FRAME_TAIL: usize = 4;
/// A length field above this is damage, not a block: it is never allocated.
const MAX_PAYLOAD: usize = 256 << 20;
/// The largest frame `stage` can write.
const MAX_FRAME: u64 = (FRAME_HEAD + MAX_PAYLOAD + FRAME_TAIL) as u64;
/// Recovery and quarantine read files through one buffer of this size, whatever the length of
/// the tail they examine (#328).
pub const RECOVERY_BUF: usize = 64 << 10;
const ENOSPC: i32 = 28;

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0x82F6_3B78
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC: [u32; 256] = crc_table();

/// CRC-32C (Castagnoli), the checksum of frames, file headers and the journal (ADR-005 section 6.2).
pub fn crc32c(data: &[u8]) -> u32 {
    !crc32c_update(!0u32, data)
}

/// Raw (un-inverted) CRC-32C state after `data`: start from `!0`, invert the result.
fn crc32c_update(state: u32, data: &[u8]) -> u32 {
    data.iter().fold(state, |c, b| {
        CRC[((c ^ u32::from(*b)) & 0xff) as usize] ^ (c >> 8)
    })
}

/// What an injection point does when it fires (see [`SegDir::probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The process dies here.
    Crash,
    /// An I/O error: the store fails stop.
    Io,
    /// The MDBX commit is applied, then the call reports an error (outcome unknown).
    IoCommitted,
}

pub trait SegFile {
    fn write_at(&mut self, off: u64, data: &[u8]) -> io::Result<()>;
    /// Fills `buf` exactly or fails with `UnexpectedEof`.
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()>;
    fn size(&self) -> io::Result<u64>;
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    fn sync_all(&mut self) -> io::Result<()>;
}

/// A datadir as the segment writer and the journal see it. Names are relative paths such as
/// `static/headers/0000000000.seg`; `dir` arguments are directory paths such as `static/headers`.
pub trait SegDir {
    type File: SegFile;
    /// A new empty file, or an existing one cut to zero. Missing parent directories are made.
    fn create(&self, name: &str) -> io::Result<Self::File>;
    fn open(&self, name: &str) -> io::Result<Option<Self::File>>;
    /// File names (not paths) in `dir`, sorted. A missing directory is empty.
    fn list(&self, dir: &str) -> io::Result<Vec<String>>;
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;
    /// Removing a missing file is not an error.
    fn remove(&self, name: &str) -> io::Result<()>;
    /// `fsync` of the directory itself: makes creates, renames and removes inside it durable.
    fn sync(&self, dir: &str) -> io::Result<()>;
    /// Identifies this boot; the `FSYNC_FAILED` marker holds it (ADR-005 section 7, P2).
    fn boot_id(&self) -> String;
    /// The store calls this at each named kill point of ADR-005. A real directory never fires;
    /// the in-memory disk of `fault` (tests only) can.
    fn probe(&self, _point: &'static str) -> Option<Probe> {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SegError {
    #[error("segment i/o: {0}")]
    Io(#[from] io::Error),
    /// ADR-005 S3: the file is not where the manifest says it ends. Fail-stop.
    #[error("segment file holds {file} bytes, the manifest says {committed}")]
    LenMismatch { file: u64, committed: u64 },
    #[error("frame of block {0} is damaged")]
    Corrupt(u64),
    #[error("block {0} lies beyond the published length")]
    Beyond(u64),
}

impl SegError {
    /// The [`Cause`] a failure of this kind puts the store in.
    pub fn cause(&self) -> Cause {
        match self {
            SegError::Io(e) if is_enospc(e) => Cause::NoSpace,
            _ => Cause::Io,
        }
    }
}

pub fn is_enospc(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ENOSPC) || e.kind() == io::ErrorKind::StorageFull
}

pub fn seg_dir(s: Segment) -> &'static str {
    match s {
        Segment::Headers => "static/headers",
        Segment::Bodies => "static/bodies",
        Segment::Receipts => "static/receipts",
    }
}

pub fn chunk_path(s: Segment, chunk_first: u64) -> String {
    format!("{}/{chunk_first:010}.seg", seg_dir(s))
}

fn idx(s: Segment) -> usize {
    s as usize - 1
}

/// `chunk_first` of the file that holds `block`.
pub fn chunk_of(block: u64, per_file: u64) -> u64 {
    block / per_file * per_file
}

/// One block's record in one segment (ADR-005 section 6.1).
pub fn frame(block: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 16);
    v.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(payload);
    v.extend_from_slice(&crc32c(&v).to_le_bytes());
    v
}

fn header_bytes(s: Segment, chunk_first: u64) -> Vec<u8> {
    let mut v = b"XSEG".to_vec();
    v.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    v.push(s as u8);
    v.push(0);
    v.extend_from_slice(&chunk_first.to_le_bytes());
    v.extend_from_slice(&crc32c(&v).to_le_bytes());
    v
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap_or([0; 4]))
}

fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))
}

/// How many complete, CRC-valid frames `b` starts with. The reference decoder for
/// [`count_frames_in`].
#[cfg(test)]
fn count_frames(mut b: &[u8]) -> u64 {
    let mut n = 0;
    while b.len() >= FRAME_HEAD {
        let len = le_u32(&b[..4]) as usize;
        let end = FRAME_HEAD + len + FRAME_TAIL;
        if len > MAX_PAYLOAD || b.len() < end || le_u32(&b[end - 4..end]) != crc32c(&b[..end - 4]) {
            break;
        }
        n += 1;
        b = &b[end..];
    }
    n
}

/// Streams `len` bytes of `f` from `off` through a [`RECOVERY_BUF`] buffer.
fn stream(
    f: &impl SegFile,
    off: u64,
    len: u64,
    buf: &mut [u8],
    mut each: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut done = 0u64;
    while done < len {
        let n = usize::try_from(len - done).map_or(buf.len(), |r| r.min(buf.len()));
        let at = off.checked_add(done).ok_or(io::ErrorKind::InvalidData)?;
        f.read_at(at, &mut buf[..n])?;
        each(&buf[..n])?;
        done += n as u64;
    }
    Ok(())
}

/// keccak256 of `len` bytes of `f` from `off`, read with a bounded buffer.
fn digest_range(f: &impl SegFile, off: u64, len: u64, buf: &mut [u8]) -> io::Result<B256> {
    let mut h = Keccak256::new();
    stream(f, off, len, buf, |b| {
        h.update(b);
        Ok(())
    })?;
    Ok(h.finalize())
}

/// [`count_frames`] over the bytes of `f` from `from` to `end`, with a bounded buffer: each frame
/// length is checked against [`MAX_PAYLOAD`] and the remaining length before any payload is read.
fn count_frames_in(f: &impl SegFile, from: u64, end: u64, buf: &mut [u8]) -> u64 {
    let mut n = 0;
    let mut at = from;
    loop {
        let left = end.saturating_sub(at);
        if left < FRAME_HEAD as u64 {
            break;
        }
        let mut head = [0u8; FRAME_HEAD];
        if f.read_at(at, &mut head).is_err() {
            break;
        }
        let len = le_u32(&head[..4]) as usize;
        let size = (FRAME_HEAD + len + FRAME_TAIL) as u64;
        if len > MAX_PAYLOAD || left < size {
            break;
        }
        let mut crc = crc32c_update(!0u32, &head);
        let body = at + FRAME_HEAD as u64;
        if stream(f, body, len as u64, buf, |b| {
            crc = crc32c_update(crc, b);
            Ok(())
        })
        .is_err()
        {
            break;
        }
        let mut tail = [0u8; FRAME_TAIL];
        if f.read_at(body + len as u64, &mut tail).is_err() || le_u32(&tail) != !crc {
            break;
        }
        n += 1;
        at += size;
    }
    n
}

/// Where a staged frame landed.
#[derive(Clone, Copy, Debug)]
pub struct Staged {
    pub start: u64,
    pub end: u64,
    /// keccak256 of the whole frame: the manifest's `tail_digest`.
    pub digest: B256,
    /// A new chunk file was created (kill point K09).
    pub created: bool,
}

/// What the scanner found.
pub struct Scanned {
    pub tails: Vec<SegmentTail>,
    /// A segment file header with another format version (refused as `FormatVersion`).
    pub header_version: Option<u16>,
}

pub struct Segs<D: SegDir> {
    pub dir: D,
    per_file: u64,
    writers: HashMap<(usize, u64), D::File>,
    /// Published end of the active chunk of each segment: (chunk first block, length).
    visible: [Option<(u64, u64)>; 3],
}

impl<D: SegDir> Segs<D> {
    pub fn new(dir: D, per_file: u64) -> Self {
        Self {
            dir,
            per_file,
            writers: HashMap::new(),
            visible: [None; 3],
        }
    }

    pub fn per_file(&self) -> u64 {
        self.per_file
    }

    /// Publication: readers see up to `m`'s lengths and nothing beyond (ADR-005 section 5, S7).
    pub fn publish(&mut self, m: &Manifest) {
        for e in &m.entries {
            self.visible[idx(e.segment)] = Some((chunk_of(e.last_block, self.per_file), e.len));
        }
    }

    /// Appends the frame of `block`. Creates the chunk file, with its header, when `block` opens a chunk.
    pub fn stage(&mut self, s: Segment, block: u64, payload: &[u8]) -> Result<Staged, SegError> {
        let (i, chunk) = (idx(s), chunk_of(block, self.per_file));
        let key = (i, chunk);
        let path = chunk_path(s, chunk);
        let mut created = false;
        let start = match self.visible[i] {
            Some((c, len)) if c == chunk => {
                if !self.writers.contains_key(&key) {
                    let f = self.dir.open(&path)?.ok_or(SegError::LenMismatch {
                        file: 0,
                        committed: len,
                    })?;
                    self.writers.insert(key, f);
                }
                let file = self.writers.get_mut(&key).ok_or(SegError::Corrupt(block))?;
                let file_len = file.size()?;
                if file_len != len {
                    return Err(SegError::LenMismatch {
                        file: file_len,
                        committed: len,
                    });
                }
                len
            }
            _ => {
                let mut f = self.dir.create(&path)?;
                f.write_at(0, &header_bytes(s, chunk))?;
                self.writers.insert(key, f);
                created = true;
                HEADER_LEN
            }
        };
        let fr = frame(block, payload);
        let file = self.writers.get_mut(&key).ok_or(SegError::Corrupt(block))?;
        file.write_at(start, &fr)?;
        Ok(Staged {
            start,
            end: start + fr.len() as u64,
            digest: keccak256(&fr),
            created,
        })
    }

    /// Writes the whole chunk file `chunk` of `s` (header, then `frames`) and syncs it. For a datadir
    /// that starts from a snapshot: the chunk holds no committed byte until a manifest names it.
    pub fn write_chunk(&mut self, s: Segment, chunk: u64, frames: &[u8]) -> Result<(), SegError> {
        let mut f = self.dir.create(&chunk_path(s, chunk))?;
        f.write_at(0, &header_bytes(s, chunk))?;
        f.write_at(HEADER_LEN, frames)?;
        f.sync_all()?;
        Ok(())
    }

    /// `fsync` of the file `block` was staged in.
    pub fn sync_file(&mut self, s: Segment, block: u64) -> Result<(), SegError> {
        let key = (idx(s), chunk_of(block, self.per_file));
        if let Some(f) = self.writers.get_mut(&key) {
            f.sync_all()?;
        }
        Ok(())
    }

    /// `fsync` of the three segment directories.
    pub fn sync_dirs(&self) -> Result<(), SegError> {
        for s in Segment::ALL {
            self.dir.sync(seg_dir(s))?;
        }
        Ok(())
    }

    /// The payload of `block`, whose frame starts at `start`. Never reads beyond the published length.
    pub fn read(&self, s: Segment, block: u64, start: u64) -> Result<Vec<u8>, SegError> {
        let chunk = chunk_of(block, self.per_file);
        let limit = match self.visible[idx(s)] {
            Some((c, len)) if c == chunk => len,
            Some((c, _)) if c < chunk => return Err(SegError::Beyond(block)),
            _ => u64::MAX,
        };
        let f = self
            .dir
            .open(&chunk_path(s, chunk))?
            .ok_or(SegError::Corrupt(block))?;
        if start.saturating_add(FRAME_HEAD as u64) > limit {
            return Err(SegError::Beyond(block));
        }
        let mut head = [0u8; FRAME_HEAD];
        f.read_at(start, &mut head)?;
        let len = le_u32(&head[..4]) as usize;
        if len > MAX_PAYLOAD || le_u64(&head[4..]) != block {
            return Err(SegError::Corrupt(block));
        }
        if start + (FRAME_HEAD + len + FRAME_TAIL) as u64 > limit {
            return Err(SegError::Beyond(block));
        }
        let mut rest = vec![0u8; len + FRAME_TAIL];
        f.read_at(start + FRAME_HEAD as u64, &mut rest)?;
        let mut all = head.to_vec();
        all.extend_from_slice(&rest[..len]);
        if le_u32(&rest[len..]) != crc32c(&all) {
            return Err(SegError::Corrupt(block));
        }
        rest.truncate(len);
        Ok(rest)
    }

    /// Cuts the chunk file `chunk` to `len`. Not durable until [`Self::sync_chunk`] and a directory sync.
    pub fn truncate(&mut self, s: Segment, chunk: u64, len: u64) -> Result<(), SegError> {
        self.writers.remove(&(idx(s), chunk));
        if let Some(mut f) = self.dir.open(&chunk_path(s, chunk))? {
            f.set_len(len)?;
        }
        Ok(())
    }

    /// `fsync` of the chunk file `chunk`.
    pub fn sync_chunk(&mut self, s: Segment, chunk: u64) -> Result<(), SegError> {
        if let Some(mut f) = self.dir.open(&chunk_path(s, chunk))? {
            f.sync_all()?;
        }
        Ok(())
    }

    /// Chunk files of `s`, by first block.
    pub fn chunks(&self, s: Segment) -> Result<Vec<u64>, SegError> {
        let mut v: Vec<u64> = self
            .dir
            .list(seg_dir(s))?
            .iter()
            .filter_map(|n| n.strip_suffix(".seg")?.parse().ok())
            .collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Unlinks one chunk file. The directory sync is the caller's.
    pub fn remove_chunk(&mut self, s: Segment, chunk: u64) -> Result<(), SegError> {
        self.writers.remove(&(idx(s), chunk));
        self.dir.remove(&chunk_path(s, chunk))?;
        Ok(())
    }

    /// Copies the bytes of `chunk` from `from` to its end into `quarantine/`, synced (ADR-005 D14).
    pub fn quarantine(&self, s: Segment, chunk: u64, from: u64, tag: &str) -> Result<(), SegError> {
        let Some(f) = self.dir.open(&chunk_path(s, chunk))? else {
            return Ok(());
        };
        let len = f.size()?.saturating_sub(from);
        let name = format!("quarantine/{}-{chunk:010}-{tag}.bin", s as u8);
        let mut q = self.dir.create(&name)?;
        let mut buf = vec![0u8; RECOVERY_BUF];
        let mut at = 0u64;
        // Any read or write error (short read, ENOSPC) returns before the copy is synced; the
        // caller truncates the source only after this returns Ok.
        stream(&f, from, len, &mut buf, |b| {
            q.write_at(at, b)?;
            at += b.len() as u64;
            Ok(())
        })?;
        if q.size()? != len {
            return Err(
                io::Error::new(io::ErrorKind::WriteZero, "quarantine copy is short").into(),
            );
        }
        q.sync_all()?;
        self.dir.sync("quarantine")?;
        Ok(())
    }

    /// P4 (ADR-005 section 7): per segment, the active file's length, whether the committed
    /// tail frame verifies, and the complete CRC-valid frames after it. `start_of(seg, block)` is
    /// the frame start offset the index holds.
    pub fn scan(
        &self,
        manifest: &ManifestState,
        start_of: impl Fn(Segment, u64) -> Option<u64>,
    ) -> Result<Scanned, SegError> {
        let mut header_version = None;
        let mut tails = Vec::new();
        let mut buf = vec![0u8; RECOVERY_BUF];
        for s in Segment::ALL {
            let none = |present| SegmentTail {
                segment: s,
                present,
                file_len: 0,
                committed_tail_ok: false,
                ahead_frames: 0,
            };
            let ManifestState::Ok(m) = manifest else {
                tails.push(none(!self.chunks(s)?.is_empty()));
                continue;
            };
            let e = &m.entries[idx(s)];
            let chunk = chunk_of(e.last_block, self.per_file);
            let Some(f) = self.dir.open(&chunk_path(s, chunk))? else {
                tails.push(none(false));
                continue;
            };
            let file_len = f.size()?;
            let mut h = [0u8; HEADER_LEN as usize];
            let mut head_ok = f.read_at(0, &mut h).is_ok() && &h[..4] == b"XSEG";
            if head_ok && le_u32(&[h[4], h[5], 0, 0]) != u32::from(FORMAT_VERSION) {
                header_version = Some(u16::from_le_bytes([h[4], h[5]]));
            }
            head_ok = head_ok && h[..] == header_bytes(s, chunk)[..];
            let ok = head_ok
                && file_len >= e.len
                && start_of(s, e.last_block).is_some_and(|start| {
                    // The committed tail is one frame: a longer span cannot match its digest.
                    start >= HEADER_LEN
                        && start < e.len
                        && e.len - start <= MAX_FRAME
                        && digest_range(&f, start, e.len - start, &mut buf)
                            .is_ok_and(|d| d == e.tail_digest)
                });
            let mut ahead = 0;
            if ok && file_len > e.len {
                ahead = count_frames_in(&f, e.len, file_len, &mut buf);
            }
            tails.push(SegmentTail {
                segment: s,
                present: true,
                file_len,
                committed_tail_ok: ok,
                ahead_frames: ahead,
            });
        }
        Ok(Scanned {
            tails,
            header_version,
        })
    }

    /// Chunk files no committed byte can live in: after the head's chunk, or wholly below the first
    /// retained block (ADR-005 sections 7 and 10).
    pub fn orphans(&self, m: &Manifest) -> Result<Vec<(Segment, u64)>, SegError> {
        let mut v = Vec::new();
        for e in &m.entries {
            let head_chunk = chunk_of(e.last_block, self.per_file);
            for c in self.chunks(e.segment)? {
                if c > head_chunk || c + self.per_file <= e.first_block {
                    v.push((e.segment, c));
                }
            }
        }
        Ok(v)
    }
}

/// The entry of `m` for `s`.
pub fn entry(m: &Manifest, s: Segment) -> &SegmentEntry {
    &m.entries[idx(s)]
}

// ---- journal (ADR-005 section 6.4)

pub const JOURNAL: &str = "journal/intent";
pub const JOURNAL_TMP: &str = "journal/intent.tmp";
pub const JOURNAL_COMMIT: u8 = 1;
pub const JOURNAL_UNWIND: u8 = 2;

pub fn journal_bytes(
    kind: u8,
    generation: u64,
    number: u64,
    hash: B256,
    payload: &[u8],
) -> Vec<u8> {
    let mut v = b"XJN1".to_vec();
    v.push(kind);
    v.extend_from_slice(&generation.to_le_bytes());
    v.extend_from_slice(&number.to_le_bytes());
    v.extend_from_slice(hash.as_slice());
    v.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    v.extend_from_slice(payload);
    v.extend_from_slice(&crc32c(&v).to_le_bytes());
    v
}

/// (kind, generation, block number) of a valid journal.
pub fn journal_parse(b: &[u8]) -> Option<(u8, u64, u64)> {
    let n = b.len();
    (n >= 4 + 1 + 8 + 8 + 32 + 4 + 4
        && &b[..4] == b"XJN1"
        && le_u32(&b[n - 4..]) == crc32c(&b[..n - 4]))
    .then(|| (b[4], le_u64(&b[5..13]), le_u64(&b[13..21])))
}

// ---- the file system

pub struct OsFile(File);

impl SegFile for OsFile {
    fn write_at(&mut self, off: u64, data: &[u8]) -> io::Result<()> {
        self.0.write_all_at(data, off)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(buf, off)
    }
    fn size(&self) -> io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

/// A datadir on disk. Holds the exclusive lock of `datadir/LOCK` while it lives (ADR-005 N5, P1); a
/// clone shares the lock (a reader of the same datadir).
#[derive(Clone)]
pub struct OsDir {
    root: PathBuf,
    _lock: Arc<File>,
}

impl OsDir {
    /// Creates the directory layout and takes the lock. Fails if another process holds it.
    pub fn open(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(root.join("LOCK"))?;
        lock.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "the datadir is in use by another process (locked)",
            ),
            fs::TryLockError::Error(e) => e,
        })?;
        let d = Self {
            root: root.to_owned(),
            _lock: Arc::new(lock),
        };
        for p in [
            "static/headers",
            "static/bodies",
            "static/receipts",
            "journal",
            "quarantine",
        ] {
            fs::create_dir_all(d.root.join(p))?;
        }
        for p in [
            "static/headers",
            "static/bodies",
            "static/receipts",
            "static",
            "journal",
            "quarantine",
            ".",
        ] {
            d.sync(p)?;
        }
        Ok(d)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SegDir for OsDir {
    type File = OsFile;
    fn create(&self, name: &str) -> io::Result<OsFile> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.root.join(name))
            .map(OsFile)
    }
    fn open(&self, name: &str) -> io::Result<Option<OsFile>> {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.join(name))
        {
            Ok(f) => Ok(Some(OsFile(f))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let rd = match fs::read_dir(self.root.join(dir)) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut v = Vec::new();
        for e in rd {
            v.push(e?.file_name().to_string_lossy().into_owned());
        }
        v.sort();
        Ok(v)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.root.join(from), self.root.join(to))
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.root.join(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
    fn sync(&self, dir: &str) -> io::Result<()> {
        File::open(self.root.join(dir))?.sync_all()
    }
    fn boot_id(&self) -> String {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_or_else(|_| "unknown".into(), |s| s.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that reports `len` bytes, all zero, and records the largest read.
    struct Huge {
        len: u64,
        max_read: std::cell::Cell<usize>,
    }

    impl SegFile for Huge {
        fn write_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
            self.max_read.set(self.max_read.get().max(buf.len()));
            if off + buf.len() as u64 > self.len {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            buf.fill(0);
            Ok(())
        }
        fn size(&self) -> io::Result<u64> {
            Ok(self.len)
        }
        fn set_len(&mut self, _: u64) -> io::Result<()> {
            Ok(())
        }
        fn sync_all(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn streamed_scan_matches_the_slice_decoder() {
        struct Mem(Vec<u8>);
        impl SegFile for Mem {
            fn write_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
                Ok(())
            }
            fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()> {
                let o = off as usize;
                let src = self
                    .0
                    .get(o..o + buf.len())
                    .ok_or(io::ErrorKind::UnexpectedEof)?;
                buf.copy_from_slice(src);
                Ok(())
            }
            fn size(&self) -> io::Result<u64> {
                Ok(self.0.len() as u64)
            }
            fn set_len(&mut self, _: u64) -> io::Result<()> {
                Ok(())
            }
            fn sync_all(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut b = frame(1, &[7u8; 100_000]);
        b.extend(frame(2, b"bbb"));
        b.extend_from_slice(&frame(3, b"c")[..5]);
        let mut buf = vec![0u8; 1000];
        for v in [b.clone(), {
            let mut c = b.clone();
            c[6] ^= 1;
            c
        }] {
            let f = Mem(v.clone());
            assert_eq!(
                count_frames_in(&f, 0, v.len() as u64, &mut buf),
                count_frames(&v)
            );
        }
        let f = Mem(b.clone());
        assert_eq!(
            digest_range(&f, 0, 1234, &mut buf).unwrap(),
            keccak256(&b[..1234])
        );
    }

    #[test]
    fn huge_suffix_reads_with_a_bounded_buffer() {
        let f = Huge {
            len: 1 << 40,
            max_read: 0.into(),
        };
        // A zero header names an empty payload; its CRC does not match zero, so nothing counts.
        let mut buf = vec![0u8; RECOVERY_BUF];
        assert_eq!(count_frames_in(&f, HEADER_LEN, f.len, &mut buf), 0);
        assert!(f.max_read.get() <= RECOVERY_BUF);
        assert!(digest_range(&f, 0, 1 << 22, &mut buf).is_ok());
        assert!(f.max_read.get() <= RECOVERY_BUF);
    }

    #[test]
    fn crc32c_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
    }

    #[test]
    fn frames_count_until_the_first_damage() {
        let mut b = frame(1, b"aa");
        b.extend(frame(2, b"bbb"));
        assert_eq!(count_frames(&b), 2);
        b.extend_from_slice(&frame(3, b"c")[..5]);
        assert_eq!(count_frames(&b), 2);
        let mut c = b.clone();
        c[6] ^= 1;
        assert_eq!(count_frames(&c), 0);
    }

    #[test]
    fn journal_roundtrip() {
        let j = journal_bytes(JOURNAL_UNWIND, 9, 4, B256::repeat_byte(1), b"");
        assert_eq!(journal_parse(&j), Some((JOURNAL_UNWIND, 9, 4)));
        let mut k = j.clone();
        k[10] ^= 1;
        assert_eq!(journal_parse(&k), None);
        assert_eq!(journal_parse(&j[..j.len() - 1]), None);
    }
}
