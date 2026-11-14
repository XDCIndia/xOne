//! `xone snapshot import`: the checks, in order, and then one write (`spec/bootstrap/import.md`).
//!
//! 1. A pins file is required ([`ImportError::NoPins`]): the file hash proves the bytes, the pins
//!    are the trust root (`spec/bootstrap-anchor.md`).
//! 2. The file version, then the manifest chunk, then [`verify_anchor`]. A manifest that fails is
//!    refused here: nothing has been written to the staging area or the store.
//! 3. The chunks are read, each checked by CRC-32C and copied to the staging area, which is by
//!    construction a whole-chunk prefix of the file. An interruption leaves the staging area at the
//!    last whole chunk; the next run reads the file from there ([`Imported::resumed_from`]).
//! 4. The trailer must name exactly the chunks that were read and the hash of the manifest.
//! 5. The content: the window `pivot_plan` asks for is held ([`ImportError::ShortWindow`]), the
//!    headers link down from the pivot, bodies and receipts match their headers, the state
//!    tables are in order and rebuild the manifest's state root, every code is referred to and
//!    hashes to its name.
//! 6. [`Store::install_snapshot`], one generation, with the anchor manifest and format version.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
};

use alloy_primitives::{keccak256, Bytes, B256, KECCAK256_EMPTY};
use alloy_rlp::Decodable;
use xone_chainspec::ChainSpec;
use xone_consensus::verify::{Body, BodyVerifier};
use xone_primitives::Header;
use xone_state::{trie::flat_root, Account, FlatWrite, MemFlat};

use super::{
    anchor::{verify_anchor, Anchor, AnchorError, Context},
    format::{
        len64, read_head, Chunk, ChunkRef, FormatError, Kind, Reader, Trailer, HEAD_LEN, MAGIC,
    },
    manifest::{AccountRow, BodyRow, CodeRow, ReceiptsRow, Side, SideRow, SlotRow},
    plan::{pivot_plan, Plan},
    retention::{Class, Error as RetentionError},
};
use crate::store::{Block, Install, Record, StateRows, Store, StoreError};

/// Where an interrupted import keeps its whole chunks: a byte prefix of the snapshot file.
pub trait Staging {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// From the first byte.
    fn reader(&self) -> std::io::Result<Box<dyn Read + '_>>;
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn truncate(&mut self, len: u64) -> std::io::Result<()>;
}

impl Staging for Vec<u8> {
    fn len(&self) -> u64 {
        len64(self.as_slice().len())
    }
    fn reader(&self) -> std::io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.as_slice()))
    }
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
    fn truncate(&mut self, len: u64) -> std::io::Result<()> {
        Vec::truncate(self, usize::try_from(len).unwrap_or(usize::MAX));
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("a pins file is required: the file hash proves the bytes, not the chain")]
    NoPins,
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("the first chunk is not the manifest")]
    NotManifest,
    #[error("manifest refused: {0}")]
    Anchor(#[from] AnchorError),
    #[error(transparent)]
    Plan(#[from] RetentionError),
    /// The file ended at a chunk boundary and has no trailer: it is not all there. The whole chunks
    /// are kept; run again with the rest.
    #[error("the file has {chunks} chunks and no trailer (next byte {offset})")]
    Incomplete { chunks: u32, offset: u64 },
    #[error("bytes after the trailer")]
    Trailing,
    #[error("trailer: {0}")]
    Trailer(&'static str),
    #[error("chunk {seq}: {why}")]
    Chunk { seq: u32, why: String },
    #[error("the snapshot holds {class:?} from block {have}, this chain needs it from {need}")]
    ShortWindow { class: Class, have: u64, need: u64 },
    #[error("{what} (block or row {at})")]
    Content { what: &'static str, at: u64 },
    #[error("state root {got} rebuilt from the tables, the manifest says {want}")]
    StateRoot { want: B256, got: B256 },
    #[error("block {number}: {why}")]
    Block { number: u64, why: String },
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(String),
}

fn io<T>(r: std::io::Result<T>) -> Result<T, ImportError> {
    r.map_err(|e| ImportError::Io(e.to_string()))
}

#[derive(Clone, Copy)]
pub struct Config<'a> {
    pub spec: &'a ChainSpec,
    pub ctx: Context<'a>,
    /// Blocks above the pivot whose state the store keeps openable.
    pub retention: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stats {
    pub chunks: u32,
    pub bytes: u64,
    pub accounts: u64,
    pub slots: u64,
    pub codes: u64,
    pub headers: u64,
    pub bodies: u64,
    /// The byte of the file this run started reading at, when a staging prefix was reused.
    pub resumed_from: Option<u64>,
}

/// A snapshot that passed every check and has not been written yet.
pub struct Verified {
    pub anchor: Anchor,
    pub side: Side,
    pub install: Install,
    pub stats: Stats,
}

pub struct Imported {
    pub anchor: Anchor,
    pub side: Side,
    pub stats: Stats,
}

fn at(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn chunk_err(seq: u32) -> impl Fn(alloy_rlp::Error) -> ImportError {
    move |e| ImportError::Chunk {
        seq,
        why: e.to_string(),
    }
}

/// Reads the chunks of the staging area; returns the references of the whole ones, and where they
/// end. A torn or damaged tail is dropped, never trusted.
fn staged(stage: &mut impl Staging) -> Result<(Vec<Chunk>, u64), ImportError> {
    if stage.is_empty() {
        return Ok((vec![], 0));
    }
    let mut out = Vec::new();
    let good = {
        let mut r = io(stage.reader())?;
        let mut head = [0u8; 6];
        if r.read_exact(&mut head).is_err() || head[..4] != MAGIC {
            0
        } else {
            let mut rd = Reader::at(r, 0, HEAD_LEN);
            while let Ok(Some(c)) = rd.read_chunk() {
                out.push(c);
            }
            rd.position().0
        }
    };
    if good == 0 {
        io(stage.truncate(0))?;
        return Ok((vec![], 0));
    }
    if good < stage.len() {
        io(stage.truncate(good))?;
    }
    Ok((out, good))
}

/// Everything up to the write: see the module comment. `input` is positioned anywhere.
pub fn verify<R: Read + Seek, St: Staging>(
    cfg: &Config<'_>,
    input: &mut R,
    stage: &mut St,
) -> Result<Verified, ImportError> {
    if cfg.ctx.pins.is_empty() {
        return Err(ImportError::NoPins);
    }
    io(input.seek(SeekFrom::Start(0)))?;
    read_head(input)?;
    let first =
        Reader::at(&mut *input, 0, HEAD_LEN)
            .read_chunk()?
            .ok_or(ImportError::Incomplete {
                chunks: 0,
                offset: HEAD_LEN,
            })?;
    if first.kind != Kind::Manifest {
        return Err(ImportError::NotManifest);
    }
    let first_raw = first.raw.clone();
    let anchor = Anchor::decode(&first.raw)?;
    verify_anchor(cfg.spec, &anchor, &cfg.ctx)?;
    let plan = pivot_plan(cfg.spec, &anchor)?;

    // From here on the staging area may be written.
    let (mut have, mut end) = staged(stage)?;
    let mut resumed_from = None;
    if have.first().is_some_and(|c| c.raw != first.raw) {
        io(stage.truncate(0))?; // staging of another snapshot
        (have, end) = (vec![], 0);
    }
    if end == 0 {
        let mut head = MAGIC.to_vec();
        head.extend_from_slice(&super::anchor::FORMAT_VERSION.to_le_bytes());
        io(stage.append(&head))?;
        io(stage.append(&first.wire))?;
        end = HEAD_LEN + len64(first.wire.len());
        have = vec![first];
    } else {
        resumed_from = Some(end);
    }
    let mut refs: Vec<ChunkRef> = have.iter().map(Chunk::reference).collect();
    let mut seq = u32::try_from(refs.len()).unwrap_or(u32::MAX);
    let mut trailer = have
        .iter()
        .find(|c| c.kind == Kind::Trailer)
        .map(|c| c.raw.clone());
    if trailer.is_none() {
        io(input.seek(SeekFrom::Start(end)))?;
        let mut rd = Reader::at(&mut *input, seq, end);
        while let Some(c) = rd.read_chunk()? {
            io(stage.append(&c.wire))?;
            seq += 1;
            if c.kind == Kind::Trailer {
                trailer = Some(c.raw);
                break;
            }
            refs.push(c.reference());
        }
        end = rd.position().0;
    } else {
        refs.pop(); // the trailer does not name itself
    }
    let Some(trailer) = trailer else {
        return Err(ImportError::Incomplete {
            chunks: seq,
            offset: end,
        });
    };
    let mut probe = [0u8; 1];
    if io(input.read(&mut probe))? != 0 {
        return Err(ImportError::Trailing);
    }
    let t: Trailer = alloy_rlp::decode_exact(&trailer).map_err(chunk_err(seq.saturating_sub(1)))?;
    if t.manifest_hash != keccak256(&first_raw) {
        return Err(ImportError::Trailer(
            "the manifest hash is not the manifest's",
        ));
    }
    if t.chunks != refs {
        return Err(ImportError::Trailer("the chunks differ from the ones read"));
    }

    let content = assemble(cfg, &anchor, &plan, &t, stage)?;
    let manifest = Bytes::from(anchor.encode());
    let mut stats = content.stats;
    stats.chunks = seq;
    stats.bytes = end;
    stats.resumed_from = resumed_from;
    Ok(Verified {
        install: Install {
            anchor: manifest,
            headers: content.headers,
            records: content.records,
            state: content.state,
            preimages: content.preimages,
            retention: cfg.retention,
            rows_in_tables: false,
        },
        side: content.side,
        anchor,
        stats,
    })
}

struct Content {
    headers: Vec<Header>,
    records: Vec<Record>,
    state: StateRows,
    preimages: Vec<(B256, Bytes)>,
    side: Side,
    stats: Stats,
}

fn decode_all<T: Decodable>(c: &Chunk) -> Result<Vec<T>, ImportError> {
    alloy_rlp::decode_exact(&c.raw).map_err(chunk_err(c.seq))
}

fn content(what: &'static str, at: u64) -> ImportError {
    ImportError::Content { what, at }
}

fn assemble(
    cfg: &Config<'_>,
    anchor: &Anchor,
    plan: &Plan,
    t: &Trailer,
    stage: &mut impl Staging,
) -> Result<Content, ImportError> {
    let pivot = anchor.number;
    let (mut accounts, mut slots, mut codes) = (Vec::new(), Vec::new(), BTreeMap::new());
    let (mut headers, mut bodies, mut receipts) = (Vec::new(), Vec::new(), Vec::new());
    let mut side: Option<SideRow> = None;
    let (mut last_a, mut last_s, mut last_c): (Option<B256>, Option<(B256, B256)>, Option<B256>) =
        (None, None, None);
    let mut rd = Reader::new(io(stage.reader())?)?;
    while let Some(c) = rd.read_chunk()? {
        match c.kind {
            Kind::Manifest | Kind::Trailer => {}
            Kind::Accounts => {
                for r in decode_all::<AccountRow>(&c)? {
                    let k = keccak256(r.address);
                    if last_a.is_some_and(|l| l >= k) {
                        return Err(content(
                            "account rows are not in ascending key order",
                            c.seq.into(),
                        ));
                    }
                    last_a = Some(k);
                    r.balance
                        .check_legal()
                        .map_err(|_| content("balance above the legal bound", c.seq.into()))?;
                    accounts.push((
                        r.address,
                        Account {
                            nonce: r.nonce,
                            balance: r.balance,
                            code_hash: r.code_hash,
                            ..Default::default()
                        },
                    ));
                }
            }
            Kind::Storage => {
                for r in decode_all::<SlotRow>(&c)? {
                    let k = (keccak256(r.address), keccak256(r.slot));
                    if last_s.is_some_and(|l| l >= k) {
                        return Err(content(
                            "slot rows are not in ascending key order",
                            c.seq.into(),
                        ));
                    }
                    last_s = Some(k);
                    if r.value.is_zero() {
                        return Err(content("a zero slot", c.seq.into()));
                    }
                    slots.push((r.address, r.slot, r.value));
                }
            }
            Kind::Codes => {
                for r in decode_all::<CodeRow>(&c)? {
                    if keccak256(&r.code) != r.hash || last_c.is_some_and(|l| l >= r.hash) {
                        return Err(content(
                            "a code that is not its hash, or out of order",
                            c.seq.into(),
                        ));
                    }
                    last_c = Some(r.hash);
                    codes.insert(r.hash, r.code);
                }
            }
            Kind::Side => {
                if side
                    .replace(alloy_rlp::decode_exact(&c.raw).map_err(chunk_err(c.seq))?)
                    .is_some()
                {
                    return Err(content("two side chunks", c.seq.into()));
                }
            }
            Kind::Headers => headers.extend(decode_all::<Header>(&c)?),
            Kind::Bodies => bodies.extend(decode_all::<BodyRow>(&c)?),
            Kind::Receipts => receipts.extend(decode_all::<ReceiptsRow>(&c)?),
        }
    }

    // ---- state
    let mut flat = MemFlat::default();
    let mut live = BTreeSet::new();
    for (a, x) in &accounts {
        live.insert(*a);
        flat.set_account(*a, Some(x.clone()));
    }
    for (a, k, v) in &slots {
        if !live.contains(a) {
            return Err(content("a slot of an account that is not in the table", 0));
        }
        flat.set_slot(*a, *k, *v);
    }
    let want_codes: BTreeSet<B256> = accounts
        .iter()
        .map(|(_, x)| x.code_hash)
        .filter(|h| *h != KECCAK256_EMPTY)
        .collect();
    if want_codes != codes.keys().copied().collect() {
        return Err(content(
            "the codes are not the codes the accounts refer to",
            0,
        ));
    }
    let got = flat_root(&flat);
    if got != anchor.state_root {
        return Err(ImportError::StateRoot {
            want: anchor.state_root,
            got,
        });
    }

    // ---- the window
    let floor = |c: Class| plan.of(c).next().map(|n| n.first);
    let need_h = floor(Class::Header).unwrap_or(pivot);
    let need_r = [Class::Body, Class::Receipt]
        .into_iter()
        .filter_map(floor)
        .min()
        .unwrap_or(pivot)
        .min(pivot);
    if t.headers_first > need_h {
        return Err(ImportError::ShortWindow {
            class: Class::Header,
            have: t.headers_first,
            need: need_h,
        });
    }
    if t.records_first > need_r {
        return Err(ImportError::ShortWindow {
            class: Class::Body,
            have: t.records_first,
            need: need_r,
        });
    }
    if t.records_first < t.headers_first {
        return Err(content("bodies start below the headers", t.records_first));
    }
    let n = pivot - t.headers_first + 1;
    if len64(headers.len()) != n
        || headers
            .iter()
            .zip(t.headers_first..)
            .any(|(h, i)| h.number != i)
    {
        return Err(content(
            "the headers are not consecutive from the trailer's first block",
            t.headers_first,
        ));
    }
    if headers.last().map(Header::hash) != Some(anchor.hash) {
        return Err(content(
            "the last header is not the manifest's checkpoint",
            pivot,
        ));
    }
    for w in headers.windows(2) {
        if w[1].parent_hash != w[0].hash() {
            return Err(content("a header does not link to its parent", w[1].number));
        }
    }
    for h in &anchor.headers {
        if h.number >= t.headers_first
            && h.number <= pivot
            && headers[at(h.number - t.headers_first)].hash() != h.hash()
        {
            return Err(content(
                "a window header differs from the proof's",
                h.number,
            ));
        }
    }
    let count = pivot - t.records_first + 1;
    if len64(bodies.len()) != count || len64(receipts.len()) != count {
        return Err(content(
            "bodies or receipts are not one per block",
            t.records_first,
        ));
    }
    let verifier = BodyVerifier::new(cfg.spec);
    let mut records = Vec::with_capacity(bodies.len());
    for ((b, r), number) in bodies.into_iter().zip(receipts).zip(t.records_first..) {
        if b.number != number || r.number != number {
            return Err(content("bodies or receipts out of order", number));
        }
        let header = headers[at(number - t.headers_first)].clone();
        let body = Body {
            transactions: b.txs,
            uncles: b.uncles,
            withdrawals: b.withdrawals,
        };
        if number > 0 {
            let bad = |e: &dyn std::fmt::Display| ImportError::Block {
                number,
                why: e.to_string(),
            };
            verifier.verify_body(&header, &body).map_err(|e| bad(&e))?;
            verifier
                .verify_receipts(&header, body.transactions.len(), &r.receipts)
                .map_err(|e| bad(&e))?;
        }
        records.push(Record {
            block: Block::new(header, body),
            receipts: r.receipts,
            diff: Default::default(),
            aux: Bytes::new(),
        });
    }

    // ---- the side data
    let side = side.ok_or(content("no side chunk", 0))?;
    let gaps: Vec<u64> = plan.of(Class::GapSnapshot).map(|n| n.first).collect();
    if side.gaps.iter().map(|g| g.number).collect::<Vec<_>>() != gaps {
        return Err(content("the gap pools are not the ones the plan names", 0));
    }
    if side
        .signs
        .iter()
        .any(|s| s.number < t.records_first || s.number > pivot)
    {
        return Err(content("sign transactions outside the window", 0));
    }
    let side = Side::of_row(side, codes);
    let (n_accounts, n_slots) = (len64(accounts.len()), len64(slots.len()));
    let (state, preimages) = StateRows::from_plain(accounts, slots);
    let stats = Stats {
        accounts: n_accounts,
        slots: n_slots,
        codes: len64(side.codes.len()),
        headers: len64(headers.len()),
        bodies: len64(records.len()),
        ..Default::default()
    };
    Ok(Content {
        headers,
        records,
        state,
        preimages,
        side,
        stats,
    })
}

/// Verifies `input` and writes it to `store`. On any error the store is untouched. The staging
/// area is cleared when the write has been made.
pub fn import<R: Read + Seek, St: Staging, S: Store>(
    cfg: &Config<'_>,
    input: &mut R,
    stage: &mut St,
    store: &mut S,
) -> Result<Imported, ImportError> {
    let v = verify(cfg, input, stage)?;
    store.install_snapshot(v.install)?;
    io(stage.truncate(0))?;
    Ok(Imported {
        anchor: v.anchor,
        side: v.side,
        stats: v.stats,
    })
}
