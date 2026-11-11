//! The node's store: `xone_sync::testkit::MemStore` (state, canonical index and the ADR-005 protocol,
//! in memory) plus the files of `<datadir>/xone/`:
//!
//! * `static/{headers,bodies,receipts}.seg`: one frame per block, `len u32 LE | number u64 LE |
//!   payload | tag`, appended and fsynced before `head.json` moves (ADR-005 sections 5 and 6.1;
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
    store::{Block, Record, Scan, Store, StoreError, Unwound},
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
        let bytes = io(&p, fs::read(&p))?;
        let len = usize::try_from(head.lens[i]).unwrap_or(usize::MAX);
        let bytes = bytes.get(..len).ok_or_else(|| {
            FileError(format!(
                "{} is shorter than the committed length {}",
                p.display(),
                head.lens[i]
            ))
        })?;
        let (mut at, mut n) = (0usize, 1u64);
        while at < bytes.len() {
            let bad = || FileError(format!("{}: damaged frame for block {n}", p.display()));
            let h = bytes.get(at..at + 12).ok_or_else(bad)?;
            let plen = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
            let number = u64::from_le_bytes([h[4], h[5], h[6], h[7], h[8], h[9], h[10], h[11]]);
            let end = at + 12 + usize::try_from(plen).unwrap_or(usize::MAX);
            let payload = bytes.get(at + 12..end).ok_or_else(bad)?;
            if number != n || bytes.get(end..end + 4) != Some(&tag(plen, number, payload)[..]) {
                return Err(bad());
            }
            part.push(payload.to_vec());
            at = end + 4;
            ends.entry(n).or_default()[i] = at as u64; // ponytail: usize is u64 here
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
        let receipts = Vec::<Bytes>::decode(&mut &rp[..]).map_err(|e| bad("receipts", e))?;
        blocks.push(Stored3 {
            header,
            body: Body {
                transactions: b.txs,
                uncles: b.uncles,
                withdrawals: None,
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
    ends: BTreeMap<u64, [u64; 3]>,
}

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
            ends: loaded.map(|l| l.ends.clone()).unwrap_or_default(),
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
            io(&path, file.sync_all())?;
            lens[i] += f.len() as u64; // ponytail: usize is u64 here
        }
        sync_dir(&self.dir.join("static"))?;
        write_head(
            &self.dir,
            &HeadFile {
                number: n,
                hash: rec.block.hash(),
                lens,
            },
        )?;
        self.ends.insert(n, lens);
        self.persisted = n;
        Ok(())
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
        Ok(())
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
    fn scan(&self) -> Scan {
        self.inner.scan()
    }
    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError> {
        self.inner.repair(actions)
    }
}
