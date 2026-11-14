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

use alloy_primitives::{keccak256, B256};

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
    !data.iter().fold(!0u32, |c, b| {
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

/// How many complete, CRC-valid frames `b` starts with.
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
        // Only the active chunk of a segment is ever appended to: retire the handles of earlier
        // chunks so a long sync holds at most one writer per segment. Their bytes stay in the page
        // cache; a deferred group commit syncs them through `sync_chunk`, which reopens by path.
        self.writers.retain(|&(si, c), _| si != i || c == chunk);
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
        let mut buf = vec![0u8; usize::try_from(f.size()?.saturating_sub(from)).unwrap_or(0)];
        f.read_at(from, &mut buf)?;
        let name = format!("quarantine/{}-{chunk:010}-{tag}.bin", s as u8);
        let mut q = self.dir.create(&name)?;
        q.write_at(0, &buf)?;
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
                    start >= HEADER_LEN && start < e.len && {
                        let mut tail = vec![0u8; usize::try_from(e.len - start).unwrap_or(0)];
                        f.read_at(start, &mut tail).is_ok() && keccak256(&tail) == e.tail_digest
                    }
                });
            let mut ahead = 0;
            if ok && file_len > e.len {
                let mut extra = vec![0u8; usize::try_from(file_len - e.len).unwrap_or(0)];
                if f.read_at(e.len, &mut extra).is_ok() {
                    ahead = count_frames(&extra);
                }
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
    /// Where the `static/` names live: `root/static`, or the path recorded in
    /// [`STATIC_PATH_FILE`] (`--datadir.ancient`).
    static_root: PathBuf,
    _lock: Arc<File>,
    _static_lock: Arc<File>,
}

/// The file in the datadir naming a segment tree kept outside it (one absolute path, one line).
pub const STATIC_PATH_FILE: &str = "segments.path";
/// The datadir's copy of its segment tree's identity.
pub const ID_FILE: &str = "segments.id";
/// The identity file inside a segment tree; it must equal the datadir's [`ID_FILE`].
pub const TREE_ID_FILE: &str = "SEGMENTS_ID";

fn take_lock(path: &Path, what: &str) -> io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    lock.try_lock().map_err(|e| match e {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("the {what} is in use by another process (locked)"),
        ),
        fs::TryLockError::Error(e) => e,
    })?;
    Ok(lock)
}

fn read_id(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `body` to `path` through a synced temporary file and a rename, then syncs the directory.
fn write_synced(path: &Path, body: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    io::Write::write_all(&mut f, body.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()
}

/// The segment tree `root` uses now: the recorded path, else `root/static`.
pub fn static_root_of(root: &Path) -> io::Result<PathBuf> {
    Ok(match read_id(&root.join(STATIC_PATH_FILE))? {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => root.join("static"),
    })
}

fn new_id(root: &Path) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seed = format!("{}|{nanos}|{}", root.display(), std::process::id());
    format!("{:x}", keccak256(seed.as_bytes()))
}

fn mismatch(tree: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "segment tree {}: {why}; pass --datadir.ancient with the path of this datadir's tree",
            tree.display()
        ),
    )
}

fn has_segment_files(tree: &Path) -> io::Result<bool> {
    for s in ["headers", "bodies", "receipts"] {
        match fs::read_dir(tree.join(s)) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    return Ok(true);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// Checks that `tree` is the segment tree of datadir `root` (ADR-005 identity, issue #225). A new
/// pair (neither id written) is stamped; any other disagreement fails without writing anything.
fn check_identity(root: &Path, tree: &Path) -> io::Result<()> {
    match (
        read_id(&root.join(ID_FILE))?,
        read_id(&tree.join(TREE_ID_FILE))?,
    ) {
        (Some(a), Some(b)) if a == b => Ok(()),
        (Some(_), Some(_)) | (None, Some(_)) => {
            Err(mismatch(tree, "it belongs to another datadir"))
        }
        (Some(_), None) => Err(mismatch(tree, "missing, empty or not a segment tree")),
        (None, None) => {
            let id = new_id(root);
            fs::create_dir_all(tree)?;
            write_synced(&tree.join(TREE_ID_FILE), &id)?;
            write_synced(&root.join(ID_FILE), &id)
        }
    }
}

/// Points datadir `root` at the segment tree `path` (`--datadir.ancient`, issue #225), before
/// [`OsDir::open`]. The tree must carry this datadir's identity; an empty or missing `path` is
/// accepted only while the datadir holds no segment files yet (a new node). A wrong path fails
/// with an [`io::ErrorKind::InvalidData`] error and changes nothing.
pub fn set_static_root(root: &Path, path: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let _lock = take_lock(&root.join("LOCK"), "datadir")?;
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let current = static_root_of(root)?;
    if current == path {
        return Ok(());
    }
    let root_id = read_id(&root.join(ID_FILE))?;
    let tree_id = read_id(&path.join(TREE_ID_FILE))?;
    match (root_id, tree_id) {
        (Some(a), Some(b)) if a == b => {}
        (_, Some(_)) => return Err(mismatch(&path, "it belongs to another datadir")),
        (Some(_), None) | (None, None) => {
            if has_segment_files(&current)? {
                return Err(mismatch(
                    &path,
                    "missing, empty or not a segment tree (move the existing tree there first)",
                ));
            }
            if has_segment_files(&path)? {
                return Err(mismatch(&path, "holds segment files without an identity"));
            }
            let id = match read_id(&root.join(ID_FILE))? {
                Some(id) => id,
                None => new_id(root),
            };
            fs::create_dir_all(&path)?;
            write_synced(&path.join(TREE_ID_FILE), &id)?;
            write_synced(&root.join(ID_FILE), &id)?;
        }
    }
    let shown = path.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the segment path is not UTF-8")
    })?;
    write_synced(&root.join(STATIC_PATH_FILE), &format!("{shown}\n"))
}

impl OsDir {
    /// Creates the directory layout and takes the lock. Fails if another process holds it. The
    /// `static/` names live in the tree [`static_root_of`] names, which must carry this datadir's
    /// identity (issue #225).
    pub fn open(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let lock = take_lock(&root.join("LOCK"), "datadir")?;
        let static_root = static_root_of(root)?;
        check_identity(root, &static_root)?;
        let static_lock = take_lock(&static_root.join("LOCK"), "segment tree")?;
        let d = Self {
            root: root.to_owned(),
            static_root,
            _lock: Arc::new(lock),
            _static_lock: Arc::new(static_lock),
        };
        for p in [
            "static/headers",
            "static/bodies",
            "static/receipts",
            "journal",
            "quarantine",
        ] {
            fs::create_dir_all(d.path(p))?;
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

    /// The file a datadir-relative name resolves to: `static/...` under the segment tree.
    fn path(&self, name: &str) -> PathBuf {
        if name == "static" {
            return self.static_root.clone();
        }
        match name.strip_prefix("static/") {
            Some(rest) => self.static_root.join(rest),
            None => self.root.join(name),
        }
    }

    /// The segment tree in use (`root/static` unless `--datadir.ancient` moved it).
    pub fn static_root(&self) -> &Path {
        &self.static_root
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
            .open(self.path(name))
            .map(OsFile)
    }
    fn open(&self, name: &str) -> io::Result<Option<OsFile>> {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path(name))
        {
            Ok(f) => Ok(Some(OsFile(f))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn list(&self, dir: &str) -> io::Result<Vec<String>> {
        let rd = match fs::read_dir(self.path(dir)) {
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
        fs::rename(self.path(from), self.path(to))
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
    fn sync(&self, dir: &str) -> io::Result<()> {
        File::open(self.path(dir))?.sync_all()
    }
    fn boot_id(&self) -> String {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_or_else(|_| "unknown".into(), |s| s.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("xone-segroot-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn separate_tree_is_used_and_checked() {
        let base = tmp("sep");
        let (root, tree) = (base.join("dd"), base.join("cold"));
        set_static_root(&root, &tree).unwrap();
        {
            let d = OsDir::open(&root).unwrap();
            assert_eq!(d.static_root(), tree.as_path());
            let mut f = d.create("static/headers/0").unwrap();
            f.write_at(0, b"h").unwrap();
        }
        assert!(tree.join("headers/0").exists());
        assert!(!root.join("static/headers/0").exists());
        // Reopening finds the recorded path without the flag.
        OsDir::open(&root).unwrap();
        // The same path again is a no-op.
        set_static_root(&root, &tree).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn wrong_or_missing_tree_fails_and_changes_nothing() {
        let base = tmp("wrong");
        let (root, tree) = (base.join("dd"), base.join("cold"));
        set_static_root(&root, &tree).unwrap();
        OsDir::open(&root)
            .unwrap()
            .create("static/bodies/0")
            .unwrap();
        let recorded = fs::read_to_string(root.join(STATIC_PATH_FILE)).unwrap();
        // An empty path while the datadir has segment files.
        let e = set_static_root(&root, &base.join("empty")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // Another datadir's tree.
        let (other, other_tree) = (base.join("dd2"), base.join("cold2"));
        set_static_root(&other, &other_tree).unwrap();
        OsDir::open(&other).unwrap();
        let e = set_static_root(&root, &other_tree).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read_to_string(root.join(STATIC_PATH_FILE)).unwrap(),
            recorded
        );
        // The recorded tree gone: open fails rather than starting with no history.
        fs::remove_dir_all(&tree).unwrap();
        let e = OsDir::open(&root).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn existing_tree_moves_out_of_the_datadir() {
        let base = tmp("move");
        let root = base.join("dd");
        OsDir::open(&root)
            .unwrap()
            .create("static/receipts/0")
            .unwrap();
        assert!(root.join(ID_FILE).exists());
        let cold = base.join("cold");
        fs::rename(root.join("static"), &cold).unwrap();
        set_static_root(&root, &cold).unwrap();
        let d = OsDir::open(&root).unwrap();
        assert!(d.open("static/receipts/0").unwrap().is_some());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn crossing_chunks_keeps_one_writer_per_segment() {
        let root = std::env::temp_dir().join(format!("xone-segs-writers-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut segs = Segs::new(OsDir::open(&root).unwrap(), 2);
        for n in 0..10u64 {
            let entries = Segment::ALL.map(|s| {
                let st = segs.stage(s, n, b"x").unwrap();
                SegmentEntry {
                    segment: s,
                    first_block: 0,
                    last_block: n,
                    len: st.end,
                    tail_digest: st.digest,
                }
            });
            segs.publish(&Manifest {
                generation: n,
                head_number: n,
                head_hash: B256::ZERO,
                entries,
            });
            assert!(segs.writers.len() <= Segment::ALL.len(), "block {n}");
        }
        assert_eq!(segs.chunks(Segment::Headers).unwrap().len(), 5);
        let _ = fs::remove_dir_all(&root);
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
