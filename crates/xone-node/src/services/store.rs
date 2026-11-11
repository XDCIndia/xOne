//! The node's store: `xone_sync::testkit::MemStore` (state, canonical index and the ADR-005 protocol,
//! in memory) plus the files of `<datadir>/xone/`:
//!
//! * `static/{headers,bodies,receipts}.seg`: one frame per block, `len u32 LE | number u64 LE |
//!   payload | tag`, appended, and fsynced before `head.json` moves (ADR-005 sections 5 and 6.1;
//!   the tag is the first 4 bytes of keccak256 of the rest, not CRC-32C, to add no crate);
//! * `head.json`: the head and the committed length of each segment, written by tmp file, fsync,
//!   rename and directory fsync. Bytes of a segment beyond its length are never read;
//! * `state/ckpt-<number>.json.gz`: the state checkpoints of the replay harness.
//!
//! ponytail: the whole chain is held in memory and a restart re-executes the blocks above the newest
//! checkpoint. The MDBX store replaces `MemStore` and these files (ADR-005); nothing above this
//! module changes.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::{Decodable, RlpDecodable, RlpEncodable};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_db::{Action, Cause, Durability};
use xone_primitives::{Header, WideBalance};
use xone_state::{Account, FlatRead, FlatWrite, MemFlat, Missing, StateRead};
use xone_sync::{
    store::{Block, Record, Scan, Stage, Store, StoreError, Unwound},
    testkit::MemStore,
};

const SEGS: [&str; 3] = ["headers", "bodies", "receipts"];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FileError(pub String);

fn io<T>(what: &Path, r: std::io::Result<T>) -> Result<T, FileError> {
    r.map_err(|e| FileError(format!("{}: {e}", what.display())))
}

#[derive(RlpEncodable, RlpDecodable)]
#[rlp(trailing)]
struct BodyFrame {
    txs: Vec<Bytes>,
    uncles: Vec<Header>,
    withdrawals: Option<Vec<Bytes>>,
}

fn tag(len: u32, number: u64, payload: &[u8]) -> [u8; 4] {
    let mut v = len.to_le_bytes().to_vec();
    v.extend_from_slice(&number.to_le_bytes());
    v.extend_from_slice(payload);
    let h = keccak256(v);
    [h[0], h[1], h[2], h[3]]
}

fn frame(number: u64, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut v = len.to_le_bytes().to_vec();
    v.extend_from_slice(&number.to_le_bytes());
    v.extend_from_slice(payload);
    v.extend_from_slice(&tag(len, number, payload));
    v
}

fn payloads(rec: &Record) -> [Vec<u8>; 3] {
    [
        alloy_rlp::encode(&rec.block.header),
        alloy_rlp::encode(BodyFrame {
            txs: rec.block.body.transactions.clone(),
            uncles: rec.block.body.uncles.clone(),
            withdrawals: rec.block.body.withdrawals.clone(),
        }),
        alloy_rlp::encode(&rec.receipts),
    ]
}

#[derive(Serialize, Deserialize)]
struct HeadFile {
    number: u64,
    hash: B256,
    lens: [u64; 3],
}

/// A block as the files hold it.
pub struct Stored3 {
    pub header: Header,
    pub body: Body,
    pub receipts: Vec<Bytes>,
}

/// What a start-up reads from the segment files: blocks `1..=number`.
pub struct Loaded {
    pub number: u64,
    pub hash: B256,
    pub blocks: Vec<Stored3>,
    ends: BTreeMap<u64, [u64; 3]>,
}

fn sync_dir(dir: &Path) -> Result<(), FileError> {
    io(dir, File::open(dir).and_then(|d| d.sync_all()))
}

fn write_head(dir: &Path, h: &HeadFile) -> Result<(), FileError> {
    let (tmp, path) = (dir.join("head.json.tmp"), dir.join("head.json"));
    let text = serde_json::to_string(h).map_err(|e| FileError(e.to_string()))?;
    let mut f = io(&tmp, File::create(&tmp))?;
    io(&tmp, f.write_all(text.as_bytes()))?;
    io(&tmp, f.sync_all())?;
    io(&path, fs::rename(&tmp, &path))?;
    sync_dir(dir)
}

fn seg_path(dir: &Path, i: usize) -> PathBuf {
    dir.join("static").join(format!("{}.seg", SEGS[i]))
}

/// Reads the committed part of the three segments. `None` for a datadir without a head file.
pub fn load(dir: &Path) -> Result<Option<Loaded>, FileError> {
    let path = dir.join("head.json");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(FileError(format!("{}: {e}", path.display()))),
    };
    let head: HeadFile =
        serde_json::from_str(&text).map_err(|e| FileError(format!("{}: {e}", path.display())))?;
    let mut parts: [Vec<Vec<u8>>; 3] = Default::default();
    let mut ends: BTreeMap<u64, [u64; 3]> = BTreeMap::new();
    for (i, part) in parts.iter_mut().enumerate() {
        let p = seg_path(dir, i);
        let file = io(&p, File::open(&p))?;
        if io(&p, file.metadata())?.len() < head.lens[i] {
            return Err(FileError(format!(
                "{} is shorter than the committed length {}",
                p.display(),
                head.lens[i]
            )));
        }
        // Read frame by frame up to the committed length: a segment can be larger than memory.
        let mut r = std::io::BufReader::new(file).take(head.lens[i]);
        let (mut at, mut n) = (0u64, 1u64);
        while at < head.lens[i] {
            let bad = || FileError(format!("{}: damaged frame for block {n}", p.display()));
            let mut h = [0u8; 12];
            r.read_exact(&mut h).map_err(|_| bad())?;
            let plen = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
            let number = u64::from_le_bytes([h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11]]);
            let mut payload = vec![0u8; usize::try_from(plen).map_err(|_| bad())?];
            let mut t = [0u8; 4];
            r.read_exact(&mut payload).map_err(|_| bad())?;
            r.read_exact(&mut t).map_err(|_| bad())?;
            if number != n || t != tag(plen, number, &payload) {
                return Err(bad());
            }
            // receipts below the unwind window stay on disk (`FileStore::receipts`)
            if i == 2 && n + super::core::RETENTION <= head.number {
                payload = Vec::new();
            }
            part.push(payload);
            at += 16 + u64::from(plen);
            ends.entry(n).or_default()[i] = at;
            n += 1;
        }
    }
    let count = parts[0].len();
    if parts[1].len() != count || parts[2].len() != count || count as u64 != head.number {
        return Err(FileError(
            "the segments and head.json disagree on the block count".into(),
        ));
    }
    let mut blocks = Vec::with_capacity(count);
    for (i, ((hp, bp), rp)) in parts[0].iter().zip(&parts[1]).zip(&parts[2]).enumerate() {
        let bad =
            |what: &str, e: alloy_rlp::Error| FileError(format!("block {}: {what}: {e}", i + 1));
        let header = Header::decode(&mut &hp[..]).map_err(|e| bad("header", e))?;
        let b = BodyFrame::decode(&mut &bp[..]).map_err(|e| bad("body", e))?;
        let receipts = if rp.is_empty() {
            vec![] // an encoded list is never empty: this one was left on disk
        } else {
            Vec::<Bytes>::decode(&mut &rp[..]).map_err(|e| bad("receipts", e))?
        };
        blocks.push(Stored3 {
            header,
            body: Body {
                transactions: b.txs,
                uncles: b.uncles,
                withdrawals: b.withdrawals,
            },
            receipts,
        });
    }
    if blocks.last().map(|b| b.header.hash()) != Some(head.hash) {
        return Err(FileError(
            "the last stored header is not the head of head.json".into(),
        ));
    }
    Ok(Some(Loaded {
        number: head.number,
        hash: head.hash,
        blocks,
        ends,
    }))
}

/// The state checkpoint of the replay harness (`conformance/harness/src/replay.rs`), plus the V2 gap
/// pools. `signs` are the sign transactions of each block as its own execution saw them.
#[derive(Serialize, Deserialize)]
pub struct Ckpt {
    pub number: u64,
    pub hash: B256,
    pub accounts: Vec<(Address, u64, Bytes, B256)>,
    pub storage: Vec<(Address, B256, U256)>,
    pub codes: BTreeMap<B256, Bytes>,
    pub signers: Vec<Address>,
    pub recents: BTreeMap<u64, Address>,
    pub signs: BTreeMap<u64, Vec<(Address, B256, bool)>>,
    #[serde(default)]
    pub gaps: Vec<(u64, Vec<Address>)>,
}

impl Ckpt {
    pub fn flat(&self) -> MemFlat {
        let mut f = MemFlat::default();
        for (a, nonce, bal, code_hash) in &self.accounts {
            f.set_account(
                *a,
                Some(Account {
                    nonce: *nonce,
                    balance: WideBalance::from_be_bytes(bal),
                    code_hash: *code_hash,
                    ..Default::default()
                }),
            );
        }
        for (a, k, v) in &self.storage {
            f.set_slot(*a, *k, *v);
        }
        f
    }

    /// The tables of `flat`, with the parts the caller owns left empty.
    pub fn of_flat(number: u64, hash: B256, flat: &MemFlat) -> Self {
        let accounts: Vec<_> = flat.accounts().collect();
        Self {
            number,
            hash,
            storage: accounts
                .iter()
                .flat_map(|(a, _)| flat.slots(*a).map(|(k, v)| (*a, k, v)))
                .collect(),
            accounts: accounts
                .into_iter()
                .map(|(a, x)| {
                    (
                        a,
                        x.nonce,
                        Bytes::from(x.balance.to_be_bytes()),
                        x.code_hash,
                    )
                })
                .collect(),
            codes: BTreeMap::new(),
            signers: vec![],
            recents: BTreeMap::new(),
            signs: BTreeMap::new(),
            gaps: vec![],
        }
    }
}

fn ckpt_dir(dir: &Path) -> PathBuf {
    dir.join("state")
}

pub fn save_ckpt(dir: &Path, c: &Ckpt) -> Result<(), FileError> {
    let d = ckpt_dir(dir);
    io(&d, fs::create_dir_all(&d))?;
    let tmp = d.join("ckpt.tmp");
    let z = GzEncoder::new(io(&tmp, File::create(&tmp))?, Compression::fast());
    serde_json::to_writer(z, c).map_err(|e| FileError(e.to_string()))?;
    io(&tmp, io(&tmp, File::open(&tmp))?.sync_all())?;
    let to = d.join(format!("ckpt-{:08}.json.gz", c.number));
    io(&to, fs::rename(&tmp, &to))
}

/// The newest checkpoint at or below `upto` that `ok` accepts (it is of this chain).
pub fn load_ckpt(dir: &Path, upto: u64, ok: impl Fn(&Ckpt) -> bool) -> Option<Ckpt> {
    let mut files: Vec<_> = fs::read_dir(ckpt_dir(dir))
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("ckpt-"))
        .map(|e| e.path())
        .collect();
    files.sort();
    files.iter().rev().find_map(|f| {
        let c: Ckpt = serde_json::from_reader(GzDecoder::new(File::open(f).ok()?)).ok()?;
        (c.number <= upto && ok(&c)).then_some(c)
    })
}

/// `MemStore` with its commits written through to the segment files.
pub struct FileStore {
    inner: MemStore,
    dir: PathBuf,
    /// Head number on disk. Blocks at or below it are replayed from the files and not written again.
    persisted: u64,
    persisted_hash: B256,
    ends: BTreeMap<u64, [u64; 3]>,
    /// The head `head.json` holds (frames above it are written but not yet fsynced), and when.
    synced: std::sync::Mutex<((u64, B256), Instant)>,
}

/// ponytail: group commit. Segments and `head.json` are fsynced at most once per this interval, at the
/// end of each import batch and at a checkpoint (`Core::sync`), and when the store is dropped; not per
/// block: six fsyncs per block capped the import at about 8 blocks
/// per second on a busy disk. A crash loses at most the last interval of blocks; their frames lie
/// beyond the committed lengths, are cut on open and fetched again. Per-batch sync if that is too much.
const GROUP_COMMIT: Duration = Duration::from_secs(1);

impl FileStore {
    pub fn new(inner: MemStore, dir: &Path, loaded: Option<&Loaded>) -> Result<Self, FileError> {
        io(&dir.join("static"), fs::create_dir_all(dir.join("static")))?;
        // Bytes beyond the committed length are a torn append or leftovers: cut them, so that the next
        // frame lands where `head.json` says the segment ends.
        let lens = loaded
            .and_then(|l| l.ends.get(&l.number).copied())
            .unwrap_or([0; 3]);
        for (i, len) in lens.iter().enumerate() {
            let p = seg_path(dir, i);
            if p.exists() {
                let f = io(&p, OpenOptions::new().write(true).open(&p))?;
                if io(&p, f.metadata())?.len() > *len {
                    io(&p, f.set_len(*len))?;
                    io(&p, f.sync_all())?;
                }
            }
        }
        Ok(Self {
            inner,
            dir: dir.to_owned(),
            persisted: loaded.map_or(0, |l| l.number),
            persisted_hash: loaded.map_or(B256::ZERO, |l| l.hash),
            ends: loaded.map(|l| l.ends.clone()).unwrap_or_default(),
            synced: std::sync::Mutex::new((
                loaded.map_or((0, B256::ZERO), |l| (l.number, l.hash)),
                Instant::now(),
            )),
        })
    }

    /// The flat tables of the head, for a checkpoint.
    pub fn head_flat(&self) -> &MemFlat {
        self.inner.head_flat()
    }

    fn append(&mut self, rec: &Record) -> Result<(), FileError> {
        let n = rec.block.number();
        if n != self.persisted + 1 {
            return Err(FileError(format!(
                "block {n} does not follow the stored head {}",
                self.persisted
            )));
        }
        let mut lens = self.ends.get(&self.persisted).copied().unwrap_or([0; 3]);
        for (i, p) in payloads(rec).iter().enumerate() {
            let path = seg_path(&self.dir, i);
            let f = frame(n, p);
            let mut file = io(
                &path,
                OpenOptions::new().create(true).append(true).open(&path),
            )?;
            io(&path, file.write_all(&f))?;
            lens[i] += f.len() as u64; // ponytail: usize is u64 here
        }
        self.ends.insert(n, lens);
        self.persisted = n;
        self.persisted_hash = rec.block.hash();
        if self.lock_synced().1.elapsed() >= GROUP_COMMIT {
            self.sync()?;
        }
        Ok(())
    }

    /// fsyncs the segments, then moves `head.json` to the last written block.
    pub fn sync(&self) -> Result<(), FileError> {
        let mut synced = self.lock_synced();
        synced.1 = Instant::now();
        let head = (self.persisted, self.persisted_hash);
        if synced.0 == head {
            return Ok(());
        }
        for i in 0..3 {
            let path = seg_path(&self.dir, i);
            io(&path, File::open(&path).and_then(|f| f.sync_all()))?;
        }
        sync_dir(&self.dir.join("static"))?;
        let lens = self.ends.get(&head.0).copied().unwrap_or([0; 3]);
        write_head(
            &self.dir,
            &HeadFile {
                number: head.0,
                hash: head.1,
                lens,
            },
        )?;
        synced.0 = head;
        Ok(())
    }

    /// The receipts of block `n`, read back from the receipts segment.
    pub fn receipts(&self, n: u64) -> Result<Vec<Bytes>, FileError> {
        let missing = || FileError(format!("no stored frame for block {n}"));
        let end = self.ends.get(&n).ok_or_else(missing)?[2];
        let start = match n {
            0 => return Err(missing()),
            1 => 0,
            _ => self.ends.get(&(n - 1)).ok_or_else(missing)?[2],
        };
        let path = seg_path(&self.dir, 2);
        let mut f = io(&path, File::open(&path))?;
        io(&path, f.seek(std::io::SeekFrom::Start(start)))?;
        let mut frame = vec![0u8; usize::try_from(end - start).map_err(|_| missing())?];
        io(&path, f.read_exact(&mut frame))?;
        let bad = || FileError(format!("{}: damaged frame for block {n}", path.display()));
        let (h, rest) = frame.split_at_checked(12).ok_or_else(bad)?;
        let (payload, t) = rest
            .split_at_checked(rest.len().saturating_sub(4))
            .ok_or_else(bad)?;
        let plen = u32::try_from(payload.len()).map_err(|_| bad())?;
        if h[4..12] != n.to_le_bytes() || t != tag(plen, n, payload) {
            return Err(bad());
        }
        Vec::<Bytes>::decode(&mut &payload[..])
            .map_err(|e| FileError(format!("block {n}: receipts: {e}")))
    }

    fn lock_synced(&self) -> std::sync::MutexGuard<'_, ((u64, B256), Instant)> {
        self.synced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cut(&mut self, number: u64) -> Result<(), FileError> {
        let lens = if number == 0 {
            [0; 3]
        } else {
            *self
                .ends
                .get(&number)
                .ok_or_else(|| FileError(format!("no stored frames for block {number}")))?
        };
        for (i, len) in lens.iter().enumerate() {
            let p = seg_path(&self.dir, i);
            let f = io(&p, OpenOptions::new().write(true).open(&p))?;
            io(&p, f.set_len(*len))?;
            io(&p, f.sync_all())?;
        }
        let hash = self.inner.canonical_hash(number).unwrap_or_default();
        write_head(&self.dir, &HeadFile { number, hash, lens })?;
        self.ends.retain(|n, _| *n <= number);
        self.persisted = number;
        self.persisted_hash = hash;
        self.lock_synced().0 = (number, hash);
        Ok(())
    }
}

impl Drop for FileStore {
    fn drop(&mut self) {
        if let Err(e) = self.sync() {
            eprintln!("store: last blocks not synced on close: {e}");
        }
    }
}

fn degraded(_: FileError) -> StoreError {
    StoreError::Degraded(Cause::Io)
}

impl Store for FileStore {
    fn head(&self) -> (u64, B256) {
        self.inner.head()
    }
    fn canonical_hash(&self, number: u64) -> Option<B256> {
        self.inner.canonical_hash(number)
    }
    fn header(&self, hash: B256) -> Option<Header> {
        self.inner.header(hash)
    }
    fn record(&self, hash: B256) -> Option<Record> {
        self.inner.record(hash)
    }
    fn stateless(&self, hash: B256) -> Option<Block> {
        self.inner.stateless(hash)
    }
    fn state_at(&self, hash: B256) -> Result<Box<dyn StateRead + '_>, Missing> {
        self.inner.state_at(hash)
    }
    fn committed(&self) -> Option<Link> {
        self.inner.committed()
    }
    fn stateless_children(&self, parent: B256) -> usize {
        self.inner.stateless_children(parent)
    }
    fn durability(&self) -> Durability {
        self.inner.durability()
    }
    fn commit_block(&mut self, rec: &Record, committed: Option<Link>) -> Result<(), StoreError> {
        self.inner.commit_block(rec, committed)?;
        if rec.block.number() > self.persisted {
            self.append(rec).map_err(degraded)?;
        }
        // the files hold every block: memory keeps the unwind window only
        self.inner
            .forget_below(rec.block.number().saturating_sub(super::core::RETENTION));
        Ok(())
    }
    fn unwind_to(&mut self, number: u64, blocks: Unwound) -> Result<(), StoreError> {
        self.inner.unwind_to(number, blocks)?;
        if number < self.persisted {
            self.cut(number).map_err(degraded)?;
        }
        Ok(())
    }
    fn put_side(&mut self, rec: &Record) -> Result<(), StoreError> {
        self.inner.put_side(rec)
    }
    fn put_stateless(&mut self, block: &Block) -> Result<(), StoreError> {
        self.inner.put_stateless(block)
    }
    // Staged sync rows (XB-14) are not file-backed: this node's chain service drives `Chain::insert`
    // directly and replays the stored blocks at start, so the pipeline's rows stay in the memory model.
    fn checkpoint(&self, stage: Stage) -> u64 {
        self.inner.checkpoint(stage)
    }
    fn staged_header(&self, number: u64) -> Option<Header> {
        self.inner.staged_header(number)
    }
    fn staged_body(&self, number: u64) -> Option<Body> {
        self.inner.staged_body(number)
    }
    fn stage_headers(&mut self, headers: &[Header]) -> Result<(), StoreError> {
        self.inner.stage_headers(headers)
    }
    fn stage_bodies(&mut self, from: u64, bodies: &[Body]) -> Result<(), StoreError> {
        self.inner.stage_bodies(from, bodies)
    }
    fn set_checkpoint(&mut self, stage: Stage, number: u64) -> Result<(), StoreError> {
        self.inner.set_checkpoint(stage, number)
    }
    fn finish(&mut self, up_to: u64) -> Result<(), StoreError> {
        self.inner.finish(up_to)
    }
    fn unwind_stages(&mut self, number: u64) -> Result<(), StoreError> {
        self.inner.unwind_stages(number)
    }
    fn scan(&self) -> Scan {
        self.inner.scan()
    }
    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError> {
        self.inner.repair(actions)
    }
}
