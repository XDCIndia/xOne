//! The node's store: `xone_sync::testkit::MemStore` (state, canonical index and the ADR-005 protocol,
//! in memory) plus the files of `<datadir>/xone/`:
//!
//! * `static/{headers,bodies,receipts}.seg`: one frame per block, `len u32 LE | number u64 LE |
//!   payload | tag`, appended and fsynced before `head.json` moves (ADR-005 sections 5 and 6.1;
//!   the tag is the first 4 bytes of keccak256 of the rest, not CRC-32C, to add no crate);
//! * `head.json`: the head and the committed length of each segment, written by tmp file, fsync,
//!   rename and directory fsync. Bytes of a segment beyond its length are never read. A datadir
//!   that was bootstrapped from a snapshot or pruned (minimal mode) starts at `first`, not at block
//!   1; a body or receipt frame below `records_from` has an empty payload; a rewrite of the
//!   segments by [`FileStore::prune`] is a new generation `gen` of the files, published by
//!   `head.json` and deleted behind it (ADR-005 section 10 P-1, P-2);
//! * `anchor.rlp`: the verified manifest of the snapshot, written before `head.json`;
//! * `state/ckpt-<number>.json.gz`: the state checkpoints of the replay harness.
//!
//! ponytail: the whole chain is held in memory and a restart re-executes the blocks above the newest
//! checkpoint. The MDBX store replaces `MemStore` and these files (ADR-005); nothing above this
//! module changes.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
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
    store::{Block, Install, Record, Scan, Stage, StateRows, Store, StoreError, Unwound},
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
struct BodyFrame {
    txs: Vec<Bytes>,
    uncles: Vec<Header>,
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
        }),
        alloy_rlp::encode(&rec.receipts),
    ]
}

fn one() -> u64 {
    1
}

#[derive(Serialize, Deserialize)]
struct HeadFile {
    number: u64,
    hash: B256,
    lens: [u64; 3],
    /// The first block of the segments; 1 for a node that followed from genesis.
    #[serde(default = "one")]
    first: u64,
    /// Bodies and receipts below this block are empty frames.
    #[serde(default = "one")]
    records_from: u64,
    #[serde(default)]
    gen: u64,
}

/// A block as the files hold it.
pub struct Stored3 {
    pub header: Header,
    pub body: Body,
    pub receipts: Vec<Bytes>,
}

/// What a start-up reads from the segment files: blocks `first..=number`.
pub struct Loaded {
    pub number: u64,
    pub hash: B256,
    pub first: u64,
    pub records_from: u64,
    gen: u64,
    /// The manifest of the snapshot the datadir was made from.
    pub anchor: Option<Bytes>,
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

fn seg_path(dir: &Path, i: usize, gen: u64) -> PathBuf {
    let name = if gen == 0 {
        format!("{}.seg", SEGS[i])
    } else {
        format!("{}.{gen}.seg", SEGS[i])
    };
    dir.join("static").join(name)
}

/// Deletes the segment files that are not generation `gen`: what a crash before `head.json`
/// (the new files) or after it (the old ones) left.
fn sweep(dir: &Path, gen: u64) {
    for i in 0..3 {
        let keep = seg_path(dir, i, gen);
        if let Ok(rd) = fs::read_dir(dir.join("static")) {
            for e in rd.filter_map(Result::ok) {
                let n = e.file_name().to_string_lossy().into_owned();
                let own = n.starts_with(SEGS[i]) && n.ends_with(".seg");
                if own && e.path() != keep {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
}

/// The frames of one segment file's committed bytes, from block `first`: number and payload.
fn frames(bytes: &[u8], first: u64, path: &Path) -> Result<Vec<(u64, std::ops::Range<usize>)>, FileError> {
    let mut out = Vec::new();
    let (mut at, mut n) = (0usize, first);
    while at < bytes.len() {
        let bad = || FileError(format!("{}: damaged frame for block {n}", path.display()));
        let h = bytes.get(at..at + 12).ok_or_else(bad)?;
        let plen = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let number = u64::from_le_bytes([h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11]]);
        let end = at + 12 + usize::try_from(plen).unwrap_or(usize::MAX);
        let payload = bytes.get(at + 12..end).ok_or_else(bad)?;
        if number != n || bytes.get(end..end + 4) != Some(&tag(plen, number, payload)[..]) {
            return Err(bad());
        }
        out.push((n, at + 12..end));
        at = end + 4;
        n += 1;
    }
    Ok(out)
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
        let p = seg_path(dir, i, head.gen);
        let bytes = io(&p, fs::read(&p))?;
        let len = usize::try_from(head.lens[i]).unwrap_or(usize::MAX);
        let bytes = bytes.get(..len).ok_or_else(|| {
            FileError(format!(
                "{} is shorter than the committed length {}",
                p.display(),
                head.lens[i]
            ))
        })?;
        for (n, range) in frames(bytes, head.first, &p)? {
            part.push(bytes[range.clone()].to_vec());
            ends.entry(n).or_default()[i] = (range.end + 4) as u64; // ponytail: usize is u64 here
        }
    }
    let count = parts[0].len();
    if parts[1].len() != count
        || parts[2].len() != count
        || count as u64 != head.number + 1 - head.first
    {
        return Err(FileError(
            "the segments and head.json disagree on the block count".into(),
        ));
    }
    let mut blocks = Vec::with_capacity(count);
    for (i, ((hp, bp), rp)) in parts[0].iter().zip(&parts[1]).zip(&parts[2]).enumerate() {
        let n = head.first + i as u64;
        let bad = |what: &str, e: alloy_rlp::Error| FileError(format!("block {n}: {what}: {e}"));
        let header = Header::decode(&mut &hp[..]).map_err(|e| bad("header", e))?;
        let (body, receipts) = if n < head.records_from {
            if !bp.is_empty() || !rp.is_empty() {
                return Err(FileError(format!("block {n}: a body below records_from")));
            }
            (Body::default(), vec![])
        } else {
            let b = BodyFrame::decode(&mut &bp[..]).map_err(|e| bad("body", e))?;
            let r = Vec::<Bytes>::decode(&mut &rp[..]).map_err(|e| bad("receipts", e))?;
            (
                Body {
                    transactions: b.txs,
                    uncles: b.uncles,
                    withdrawals: None,
                },
                r,
            )
        };
        blocks.push(Stored3 {
            header,
            body,
            receipts,
        });
    }
    if blocks.last().map(|b| b.header.hash()) != Some(head.hash) {
        return Err(FileError(
            "the last stored header is not the head of head.json".into(),
        ));
    }
    let anchor = match fs::read(dir.join("anchor.rlp")) {
        Ok(b) => Some(Bytes::from(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(FileError(format!("anchor.rlp: {e}"))),
    };
    Ok(Some(Loaded {
        number: head.number,
        hash: head.hash,
        first: head.first,
        records_from: head.records_from,
        gen: head.gen,
        anchor,
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

/// Minimal mode keeps the newest state checkpoint only: the older ones are state of blocks the node
/// has pruned.
pub fn drop_ckpts_below(dir: &Path, number: u64) {
    if let Ok(rd) = fs::read_dir(ckpt_dir(dir)) {
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix("ckpt-")
                .and_then(|r| r.strip_suffix(".json.gz"))
                .and_then(|r| r.parse::<u64>().ok());
            if n.is_some_and(|n| n < number) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
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
    ends: BTreeMap<u64, [u64; 3]>,
    first: u64,
    records_from: u64,
    gen: u64,
}

impl FileStore {
    pub fn new(inner: MemStore, dir: &Path, loaded: Option<&Loaded>) -> Result<Self, FileError> {
        io(&dir.join("static"), fs::create_dir_all(dir.join("static")))?;
        let gen = loaded.map_or(0, |l| l.gen);
        sweep(dir, gen);
        // Bytes beyond the committed length are a torn append or leftovers: cut them, so that the next
        // frame lands where `head.json` says the segment ends.
        let lens = loaded
            .and_then(|l| l.ends.get(&l.number).copied())
            .unwrap_or([0; 3]);
        for (i, len) in lens.iter().enumerate() {
            let p = seg_path(dir, i, gen);
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
            ends: loaded.map(|l| l.ends.clone()).unwrap_or_default(),
            first: loaded.map_or(1, |l| l.first),
            records_from: loaded.map_or(1, |l| l.records_from),
            gen,
        })
    }

    /// The flat tables of the head, for a checkpoint.
    pub fn head_flat(&self) -> &MemFlat {
        self.inner.head_flat()
    }

    fn head_file(&self, number: u64, hash: B256, lens: [u64; 3]) -> HeadFile {
        HeadFile {
            number,
            hash,
            lens,
            first: self.first,
            records_from: self.records_from,
            gen: self.gen,
        }
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
            let path = seg_path(&self.dir, i, self.gen);
            let f = frame(n, p);
            let mut file = io(
                &path,
                OpenOptions::new().create(true).append(true).open(&path),
            )?;
            io(&path, file.write_all(&f))?;
            io(&path, file.sync_all())?;
            lens[i] += f.len() as u64; // ponytail: usize is u64 here
        }
        sync_dir(&self.dir.join("static"))?;
        write_head(&self.dir, &self.head_file(n, rec.block.hash(), lens))?;
        self.ends.insert(n, lens);
        self.persisted = n;
        Ok(())
    }

    fn cut(&mut self, number: u64) -> Result<(), FileError> {
        let lens = if number + 1 == self.first {
            [0; 3]
        } else {
            *self
                .ends
                .get(&number)
                .ok_or_else(|| FileError(format!("no stored frames for block {number}")))?
        };
        for (i, len) in lens.iter().enumerate() {
            let p = seg_path(&self.dir, i, self.gen);
            let f = io(&p, OpenOptions::new().write(true).open(&p))?;
            io(&p, f.set_len(*len))?;
            io(&p, f.sync_all())?;
        }
        let hash = self.inner.canonical_hash(number).unwrap_or_default();
        write_head(&self.dir, &self.head_file(number, hash, lens))?;
        self.ends.retain(|n, _| *n <= number);
        self.persisted = number;
        Ok(())
    }

    /// Writes the three segments of generation `gen` holding `blocks` (number, header payload, body
    /// payload, receipts payload) from `first`, and publishes them with `head.json`. The files of
    /// the generation before are deleted behind it.
    fn publish(
        &mut self,
        gen: u64,
        first: u64,
        records_from: u64,
        blocks: &[(u64, Vec<u8>, Vec<u8>, Vec<u8>)],
        head: (u64, B256),
    ) -> Result<(), FileError> {
        let mut files: [Vec<u8>; 3] = Default::default();
        let mut ends = BTreeMap::new();
        for (n, h, b, r) in blocks {
            for (i, p) in [h, b, r].into_iter().enumerate() {
                files[i].extend_from_slice(&frame(*n, p));
                ends.entry(*n).or_insert([0u64; 3])[i] = files[i].len() as u64; // ponytail: usize is u64 here
            }
        }
        for (i, bytes) in files.iter().enumerate() {
            let p = seg_path(&self.dir, i, gen);
            let mut f = io(&p, File::create(&p))?;
            io(&p, f.write_all(bytes))?;
            io(&p, f.sync_all())?;
        }
        sync_dir(&self.dir.join("static"))?;
        let lens = ends.get(&head.0).copied().unwrap_or([0; 3]);
        let old = (self.gen, self.first, self.records_from);
        (self.gen, self.first, self.records_from) = (gen, first, records_from);
        if let Err(e) = write_head(&self.dir, &self.head_file(head.0, head.1, lens)) {
            (self.gen, self.first, self.records_from) = old;
            return Err(e);
        }
        sweep(&self.dir, gen);
        self.ends = ends;
        self.persisted = head.0;
        Ok(())
    }

    /// Rewrites the segments from the files as they are: frames below `first` dropped, bodies and
    /// receipts below `records_from` emptied.
    fn rewrite(&mut self, first: u64, records_from: u64) -> Result<(), FileError> {
        let mut parts: [Vec<(u64, Vec<u8>)>; 3] = Default::default();
        for (i, part) in parts.iter_mut().enumerate() {
            let p = seg_path(&self.dir, i, self.gen);
            let bytes = io(&p, fs::read(&p))?;
            let len = self.ends.get(&self.persisted).map_or(0, |e| e[i]);
            let bytes = &bytes[..usize::try_from(len).unwrap_or(0).min(bytes.len())];
            for (n, r) in frames(bytes, self.first, &p)? {
                if n >= first {
                    part.push((n, bytes[r].to_vec()));
                }
            }
        }
        let blocks: Vec<_> = (0..parts[0].len())
            .map(|k| {
                let n = parts[0][k].0;
                let keep = |i: usize| {
                    if i > 0 && n < records_from {
                        vec![]
                    } else {
                        parts[i][k].1.clone()
                    }
                };
                (n, keep(0), keep(1), keep(2))
            })
            .collect();
        let head = (self.persisted, self.inner.canonical_hash(self.persisted).unwrap_or_default());
        self.publish(self.gen + 1, first, records_from, &blocks, head)
    }

    /// The files of a datadir that was just installed: every header, the bodies and receipts of the
    /// records, empty frames for the blocks between the two floors; then the anchor; then
    /// `head.json`, which is what makes the datadir exist.
    fn write_install(&mut self, s: &Install) -> Result<(), FileError> {
        let first = s.headers.first().map_or(1, |h| h.number);
        let records_from = s.records.first().map_or(first, |r| r.block.number());
        let by_number: BTreeMap<u64, &Record> =
            s.records.iter().map(|r| (r.block.number(), r)).collect();
        let blocks: Vec<_> = s
            .headers
            .iter()
            .map(|h| {
                let (b, r) = by_number
                    .get(&h.number)
                    .map(|rec| {
                        let p = payloads(rec);
                        (p[1].clone(), p[2].clone())
                    })
                    .unwrap_or_default();
                (h.number, alloy_rlp::encode(h), b, r)
            })
            .collect();
        let tmp = self.dir.join("anchor.rlp.tmp");
        let mut f = io(&tmp, File::create(&tmp))?;
        io(&tmp, f.write_all(&s.anchor))?;
        io(&tmp, f.sync_all())?;
        let to = self.dir.join("anchor.rlp");
        io(&to, fs::rename(&tmp, &to))?;
        let head = s.headers.last().map_or((0, B256::ZERO), |h| (h.number, h.hash()));
        self.publish(0, first, records_from, &blocks, head)
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
    fn state_rows(&self, hash: B256) -> Result<StateRows, Missing> {
        self.inner.state_rows(hash)
    }
    fn anchor(&self) -> Option<Bytes> {
        self.inner.anchor()
    }
    fn install_snapshot(&mut self, s: Install) -> Result<(), StoreError> {
        if self.persisted != 0 || self.dir.join("head.json").exists() {
            return Err(StoreError::NotEmpty);
        }
        self.inner.install_snapshot(s.clone())?;
        self.write_install(&s).map_err(degraded)
    }
    fn prune(&mut self, headers_from: u64, records_from: u64) -> Result<(), StoreError> {
        self.inner.prune(headers_from, records_from)?;
        let (h, r) = (headers_from.max(self.first), records_from.max(self.records_from));
        if (h, r) != (self.first, self.records_from) {
            self.rewrite(h, r).map_err(degraded)?;
        }
        Ok(())
    }
    fn scan(&self) -> Scan {
        self.inner.scan()
    }
    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError> {
        self.inner.repair(actions)
    }
}
