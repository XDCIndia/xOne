//! In-memory store that runs the ADR-005 protocol step by step (sections 5, 7 and 8) over a
//! simulated disk, and can crash or fail at every kill point. `MemStore::crash` returns the disk a
//! restart would find; `MemStore::open` is the process start on it.
//!
//! What is modelled: the journal with its tmp file, rename and directory sync; three segment files
//! per chunk with frames, synced length and directory entry; one atomic MDBX commit (the manifest,
//! generation, state, canonical index, block tables and the committed block all change together or
//! not at all). Segment payloads are the real header RLP, and sized stand-ins for body and receipts:
//! reads come from the MDBX side, the frames carry lengths and digests so that `reconcile` sees real
//! tails. Not modelled: CRC, MDBX internals, F6 retry behaviour (a failed sync is fail-stop here).

use alloy_primitives::{keccak256, Bytes, B256};
use std::collections::{BTreeMap, HashMap};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_db::{
    Action, Cause, Durability, Manifest, ManifestState, Segment, SegmentEntry, SegmentTail,
};
use xone_primitives::Header;
use xone_state::{
    trie::flat_root, CommitError, FlatWrite, MemFlat, Missing, StateDiff, StateProvider, StateRead,
};

use crate::store::{
    Block, Install, KillPoint, Record, Scan, Stage, StateRows, Store, StoreError, Unwound,
};

const SEGS: [Segment; 3] = [Segment::Headers, Segment::Bodies, Segment::Receipts];
const FRAME_HEAD: usize = 12;

fn idx(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn len_of(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// What the armed kill point does when it is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `kill -9`: everything passed to `write` survives.
    Kill,
    /// Power loss: unsynced data and directory entries are lost.
    Power,
    /// An I/O error: the store fails stop (ADR-005 section 9). The disk is as after `Kill`.
    Io,
    /// K15 only: the commit is applied, then the call reports an error (outcome unknown).
    IoCommitted,
}

/// How a crash treats data that was not synced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    Kill9,
    PowerLoss,
    /// Power loss that keeps this many bytes of the unsynced tail of each segment file (a torn write).
    Torn(usize),
}

struct File {
    first_block: u64,
    bytes: Vec<u8>,
    synced: usize,
    in_dir: bool,
}

struct Mdbx {
    generation: u64,
    manifest: Option<Vec<u8>>,
    canonical: BTreeMap<u64, B256>,
    records: HashMap<B256, Record>,
    stateless: HashMap<B256, Block>,
    state: StateProvider<MemFlat>,
    committed: Option<Link>,
    /// (segment index, block) to (first block of the file, frame start, frame end).
    segidx: BTreeMap<(usize, u64), (u64, usize, usize)>,
    /// Staged sync rows and the checkpoints of `Stage::{Headers, Bodies, Senders, Finish}`.
    staged_headers: BTreeMap<u64, Header>,
    staged_bodies: BTreeMap<u64, Body>,
    ckpt: BTreeMap<Stage, u64>,
    /// Headers below the record floor of a pruned or bootstrapped datadir.
    headers_only: HashMap<B256, Header>,
    /// The `Meta[anchor]` row of a datadir bootstrapped from a snapshot.
    anchor: Option<Bytes>,
}

/// The persistent image: what survives a process crash.
pub struct Disk {
    mdbx: Mdbx,
    segs: [Vec<File>; 3],
    journal_tmp: Option<(Vec<u8>, bool)>,
    /// Renamed journal and whether the directory entry is synced.
    journal: Option<(Vec<u8>, bool)>,
    quarantine: Vec<u8>,
    blocks_per_file: u64,
}

struct Armed {
    point: KillPoint,
    fault: Fault,
    skip: usize,
}

pub struct MemStore {
    d: Disk,
    armed: Option<Armed>,
    dead: Option<StoreError>,
    cut: Cut,
    committed_on_error: bool,
    log: Vec<KillPoint>,
    /// `forget_below` has dropped the records below this block.
    forgotten: u64,
}

fn journal_bytes(kind: u8, generation: u64, number: u64, hash: B256) -> Vec<u8> {
    let mut v = b"XJN1".to_vec();
    v.push(kind);
    v.extend_from_slice(&generation.to_le_bytes());
    v.extend_from_slice(&number.to_le_bytes());
    v.extend_from_slice(hash.as_slice());
    v
}

fn frame(block: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(payload.len())
        .unwrap_or(u32::MAX)
        .to_le_bytes()
        .to_vec();
    v.extend_from_slice(&block.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn payloads(rec: &Record) -> [Vec<u8>; 3] {
    let body = rec
        .block
        .body
        .transactions
        .iter()
        .fold(Vec::new(), |mut v, t| {
            v.extend_from_slice(t);
            v
        });
    let receipts = rec.receipts.iter().fold(Vec::new(), |mut v, r| {
        v.extend_from_slice(r);
        v
    });
    [alloy_rlp::encode(&rec.block.header), body, receipts]
}

fn pivot_first(records: &[Record]) -> u64 {
    records.first().map_or(0, |r| r.block.number())
}

impl Disk {
    fn manifest(&self) -> Option<Manifest> {
        Manifest::decode(self.mdbx.manifest.as_ref()?).ok()
    }

    /// Fault injection for restart tests: the canonical index entry is lost.
    pub fn damage_canonical(&mut self, number: u64) {
        self.mdbx.canonical.remove(&number);
    }

    /// Fault injection: the block record (header, body, diff) of a block is lost.
    pub fn damage_record(&mut self, hash: B256) {
        self.mdbx.records.remove(&hash);
    }

    /// Fault injection: the manifest row of the `Meta` table is lost.
    pub fn damage_manifest(&mut self) {
        self.mdbx.manifest = None;
    }

    /// Fault injection: one byte of the manifest is flipped.
    pub fn flip_manifest_byte(&mut self, at: usize) {
        if let Some(b) = self.mdbx.manifest.as_mut().and_then(|m| m.get_mut(at)) {
            *b ^= 1;
        }
    }

    /// Fault injection: the files of one segment (0 headers, 1 bodies, 2 receipts) are deleted.
    pub fn damage_segment_file(&mut self, segment: usize) {
        self.segs[segment].clear();
    }

    /// Bytes kept by the repair of a far-ahead tail (ADR-005 row D14).
    pub fn quarantined(&self) -> usize {
        self.quarantine.len()
    }
}

impl MemStore {
    /// A datadir that holds one committed block, the anchor, with `flat` as its state: the genesis
    /// commit of the node, or a trusted starting point. `retention` is the state window in blocks;
    /// `blocks_per_file` the segment chunk size.
    pub fn with_anchor(
        header: Header,
        flat: MemFlat,
        retention: u64,
        blocks_per_file: u64,
    ) -> Self {
        let (number, block) = (header.number, Block::new(header, Default::default()));
        let hash = block.hash();
        let rec = Record {
            block,
            receipts: Vec::new(),
            diff: StateDiff::default(),
            aux: Default::default(),
        };
        let mut segs: [Vec<File>; 3] = Default::default();
        let mut segidx = BTreeMap::new();
        let mut entries = [SegmentEntry {
            segment: Segment::Headers,
            first_block: number,
            last_block: number,
            len: 0,
            tail_digest: B256::ZERO,
        }; 3];
        for (i, p) in payloads(&rec).iter().enumerate() {
            let f = frame(number, p);
            entries[i] = SegmentEntry {
                segment: SEGS[i],
                first_block: number,
                last_block: number,
                len: idx(f.len()),
                tail_digest: keccak256(&f),
            };
            segidx.insert((i, number), (number, 0, f.len()));
            segs[i].push(File {
                first_block: number,
                synced: f.len(),
                bytes: f,
                in_dir: true,
            });
        }
        let manifest = Manifest {
            generation: 1,
            head_number: number,
            head_hash: hash,
            entries,
        };
        Self::open(Disk {
            mdbx: Mdbx {
                generation: 1,
                manifest: Some(manifest.encode()),
                canonical: BTreeMap::from([(number, hash)]),
                records: HashMap::from([(hash, rec)]),
                stateless: HashMap::new(),
                state: StateProvider::new(flat, number, hash, retention),
                committed: None,
                segidx,
                staged_headers: BTreeMap::new(),
                staged_bodies: BTreeMap::new(),
                ckpt: BTreeMap::new(),
                headers_only: HashMap::new(),
                anchor: None,
            },
            segs,
            journal_tmp: None,
            journal: None,
            quarantine: Vec::new(),
            blocks_per_file,
        })
    }

    /// Process start on a disk. Nothing is checked: that is `resume`.
    pub fn open(d: Disk) -> Self {
        Self {
            d,
            armed: None,
            dead: None,
            cut: Cut::Kill9,
            committed_on_error: false,
            log: Vec::new(),
            forgotten: 0,
        }
    }

    /// A new, empty datadir.
    pub fn empty(blocks_per_file: u64) -> Self {
        Self::open(Disk {
            mdbx: Mdbx {
                generation: 0,
                manifest: None,
                canonical: BTreeMap::new(),
                records: HashMap::new(),
                stateless: HashMap::new(),
                state: StateProvider::new(MemFlat::default(), 0, B256::ZERO, 0),
                committed: None,
                segidx: BTreeMap::new(),
                staged_headers: BTreeMap::new(),
                staged_bodies: BTreeMap::new(),
                ckpt: BTreeMap::new(),
                headers_only: HashMap::new(),
                anchor: None,
            },
            segs: Default::default(),
            journal_tmp: None,
            journal: None,
            quarantine: Vec::new(),
            blocks_per_file,
        })
    }

    /// The next time `point` is reached (after `skip` earlier visits), `fault` happens.
    pub fn arm(&mut self, point: KillPoint, fault: Fault, skip: usize) {
        self.armed = Some(Armed { point, fault, skip });
    }

    /// Kill points passed so far, in order: the write log.
    pub fn log(&self) -> &[KillPoint] {
        &self.log
    }

    /// The disk a restart finds, cut as `cut` says.
    pub fn crash(self, cut: Cut) -> Disk {
        let mut d = self.d;
        if cut != Cut::Kill9 {
            for files in &mut d.segs {
                files.retain(|f| f.in_dir);
                for f in files.iter_mut() {
                    let keep = match cut {
                        Cut::Torn(k) => f.synced.saturating_add(k).min(f.bytes.len()),
                        _ => f.synced,
                    };
                    f.bytes.truncate(keep);
                }
            }
            if d.journal_tmp.as_ref().is_some_and(|(_, synced)| !synced) {
                d.journal_tmp = None;
            }
            if let Some((b, false)) = d.journal.take() {
                d.journal_tmp = Some((b, true)); // the rename did not reach the disk
            }
        }
        d
    }

    /// The cut the armed fault implies: `Power` loses unsynced data, the others keep it.
    pub fn natural_cut(&self) -> Cut {
        self.cut
    }

    /// The flat tables of the head, for a node that writes its own state checkpoints.
    /// For an owner that keeps the chain on its own files (the node's `FileStore`): drops the records
    /// of canonical blocks below `number` and the bytes of segment files that end below it, so that
    /// memory follows the retained window instead of the chain. Those blocks can no longer be read
    /// here, unwound or replayed from this store.
    pub fn forget_below(&mut self, number: u64) {
        let m = &mut self.d.mdbx;
        for (_, h) in m.canonical.range(self.forgotten..number) {
            m.records.remove(h);
        }
        self.forgotten = self.forgotten.max(number);
        for files in &mut self.d.segs {
            let starts: Vec<u64> = files.iter().map(|f| f.first_block).collect();
            for (f, next) in files.iter_mut().zip(starts.iter().skip(1)) {
                if *next <= number && !f.bytes.is_empty() {
                    f.bytes = Vec::new();
                }
            }
        }
    }

    pub fn head_flat(&self) -> &MemFlat {
        self.d.mdbx.state.flat()
    }

    /// State root over the whole flat state (for tests: equals the head's `state_root`).
    pub fn flat_root(&self) -> B256 {
        flat_root(self.d.mdbx.state.flat())
    }

    /// Numbers of the canonical index, ascending.
    pub fn canonical_numbers(&self) -> Vec<u64> {
        self.d.mdbx.canonical.keys().copied().collect()
    }

    /// Hashes of the blocks stored as side blocks with state, and of the stateless ones.
    pub fn side_hashes(&self) -> Vec<B256> {
        let m = &self.d.mdbx;
        let mut v: Vec<B256> = m
            .records
            .iter()
            .filter(|(h, r)| m.canonical.get(&r.block.number()) != Some(*h))
            .map(|(h, _)| *h)
            .chain(m.stateless.keys().copied())
            .collect();
        v.sort();
        v
    }

    fn alive(&self) -> Result<(), StoreError> {
        match &self.dead {
            None => Ok(()),
            Some(StoreError::Crashed(k)) => Err(StoreError::Crashed(*k)),
            Some(_) => Err(StoreError::Degraded(Cause::Io)),
        }
    }

    fn fail_stop(&mut self) -> StoreError {
        self.dead = Some(StoreError::Degraded(Cause::Io));
        StoreError::Degraded(Cause::Io)
    }

    /// One kill point. The armed fault fires here.
    fn step(&mut self, point: KillPoint) -> Result<(), StoreError> {
        self.log.push(point);
        let Some(a) = self.armed.as_mut().filter(|a| a.point == point) else {
            return Ok(());
        };
        if a.skip > 0 {
            a.skip -= 1;
            return Ok(());
        }
        let fault = a.fault;
        self.armed = None;
        self.cut = if fault == Fault::Power {
            Cut::PowerLoss
        } else {
            Cut::Kill9
        };
        self.committed_on_error = fault == Fault::IoCommitted;
        let e = match fault {
            Fault::Kill | Fault::Power => StoreError::Crashed(point),
            Fault::Io | Fault::IoCommitted => StoreError::Degraded(Cause::Io),
        };
        self.dead = Some(match &e {
            StoreError::Crashed(k) => StoreError::Crashed(*k),
            _ => StoreError::Degraded(Cause::Io),
        });
        Err(e)
    }

    fn write_journal(
        &mut self,
        kind: u8,
        number: u64,
        hash: B256,
        first: KillPoint,
        second: KillPoint,
    ) -> Result<(), StoreError> {
        let b = journal_bytes(kind, self.d.mdbx.generation + 1, number, hash);
        self.d.journal_tmp = Some((b, false));
        self.step(first)?;
        if let Some((_, s)) = self.d.journal_tmp.as_mut() {
            *s = true; // fsync
        }
        if first == KillPoint::K02 {
            self.step(KillPoint::K03)?;
        }
        self.d.journal = self.d.journal_tmp.take().map(|(b, _)| (b, false));
        self.step(second)?;
        if let Some((_, dir)) = self.d.journal.as_mut() {
            *dir = true; // fsync of the journal directory
        }
        Ok(())
    }

    fn apply_commit(
        &mut self,
        rec: &Record,
        committed: Option<Link>,
        ends: [(u64, usize, usize); 3],
        manifest: &Manifest,
    ) -> Result<(), CommitError> {
        let b = &rec.block;
        let m = &mut self.d.mdbx;
        // The one fallible step goes first: the rest of the transaction is plain map writes.
        m.state.commit_block(b.hash(), b.parent(), &rec.diff)?;
        m.canonical.insert(b.number(), b.hash());
        m.records.insert(b.hash(), rec.clone());
        m.stateless.remove(&b.hash());
        if committed.is_some() {
            m.committed = committed;
        }
        for (i, e) in ends.iter().enumerate() {
            m.segidx.insert((i, b.number()), *e);
        }
        m.generation += 1;
        m.manifest = Some(manifest.encode());
        Ok(())
    }
}

impl Store for MemStore {
    fn head(&self) -> (u64, B256) {
        self.d
            .manifest()
            .map_or((0, B256::ZERO), |m| (m.head_number, m.head_hash))
    }

    fn canonical_hash(&self, number: u64) -> Option<B256> {
        self.d.mdbx.canonical.get(&number).copied()
    }

    fn header(&self, hash: B256) -> Option<Header> {
        let m = &self.d.mdbx;
        m.records
            .get(&hash)
            .map(|r| r.block.header.clone())
            .or_else(|| m.stateless.get(&hash).map(|b| b.header.clone()))
            .or_else(|| m.headers_only.get(&hash).cloned())
    }

    fn record(&self, hash: B256) -> Option<Record> {
        self.d.mdbx.records.get(&hash).cloned()
    }

    fn stateless(&self, hash: B256) -> Option<Block> {
        self.d.mdbx.stateless.get(&hash).cloned()
    }

    fn state_at(&self, hash: B256) -> Result<Box<dyn StateRead + '_>, Missing> {
        let m = &self.d.mdbx;
        let mut diffs: Vec<&StateDiff> = Vec::new();
        let mut cur = hash;
        loop {
            if let Ok(view) = m.state.at(cur) {
                return Ok(Box::new(diffs.iter().rev().fold(view, |v, d| v.child(d))));
            }
            // A canonical block outside the retained window has no state: it is not a side block.
            let r = m
                .records
                .get(&cur)
                .filter(|r| m.canonical.get(&r.block.number()) != Some(&cur))
                .ok_or(Missing::Block(hash))?;
            diffs.push(&r.diff);
            cur = r.block.parent();
        }
    }

    fn committed(&self) -> Option<Link> {
        self.d.mdbx.committed
    }

    fn stateless_children(&self, parent: B256) -> usize {
        self.d
            .mdbx
            .stateless
            .values()
            .filter(|b| b.parent() == parent)
            .count()
    }

    fn durability(&self) -> Durability {
        match self.dead {
            None => Durability::Durable,
            Some(_) => Durability::Degraded(Cause::Io),
        }
    }

    fn commit_block(&mut self, rec: &Record, committed: Option<Link>) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        let b = &rec.block;
        let (head_number, head_hash) = self.head();
        if b.parent() != head_hash || b.number() != head_number + 1 {
            return Err(CommitError::NotExtending {
                head: head_hash,
                parent: b.parent(),
            }
            .into());
        }
        for a in rec.diff.accounts.values().flatten() {
            a.balance.check_legal().map_err(CommitError::from)?;
        }
        let Some(old) = self.d.manifest() else {
            return Err(StoreError::Unwind(head_number));
        };
        // S1 journal
        self.step(K01)?;
        self.write_journal(1, b.number(), b.hash(), K02, K04)?;
        // S2 execute: an open MDBX transaction, no durable effect
        self.step(K05)?;
        // S3 stage
        let mut ends = [(0u64, 0usize, 0usize); 3];
        let mut created = false;
        for (i, p) in payloads(rec).iter().enumerate() {
            let files = &mut self.d.segs[i];
            let committed_len = len_of(old.entries[i].len);
            if files.last().map(|f| f.bytes.len()) != Some(committed_len) {
                return Err(self.fail_stop()); // ADR-005 S3: file_len != len is fail-stop
            }
            let first = files.last().map_or(b.number(), |f| f.first_block);
            if b.number() - first >= self.d.blocks_per_file {
                files.push(File {
                    first_block: b.number(),
                    bytes: Vec::new(),
                    synced: 0,
                    in_dir: false,
                });
                created = true;
            }
            if let Some(f) = files.last_mut() {
                let start = f.bytes.len();
                f.bytes.extend_from_slice(&frame(b.number(), p));
                ends[i] = (f.first_block, start, f.bytes.len());
            }
            self.step([K06, K07, K08][i])?;
        }
        if created {
            self.step(K09)?;
        }
        // S4 sync
        for i in 0..3 {
            if let Some(f) = self.d.segs[i].last_mut() {
                f.synced = f.bytes.len();
            }
            self.step([K10, K11, K12][i])?;
        }
        for files in &mut self.d.segs {
            for f in files.iter_mut() {
                f.in_dir = true;
            }
        }
        self.step(K13)?;
        // S5 transaction
        let mut entries = old.entries;
        for (i, e) in entries.iter_mut().enumerate() {
            let (first, start, end) = ends[i];
            let digest = self.d.segs[i]
                .iter()
                .find(|f| f.first_block == first)
                .map_or(B256::ZERO, |f| keccak256(&f.bytes[start..end]));
            *e = SegmentEntry {
                last_block: b.number(),
                len: idx(end),
                tail_digest: digest,
                ..*e
            };
        }
        let manifest = Manifest {
            generation: self.d.mdbx.generation + 1,
            head_number: b.number(),
            head_hash: b.hash(),
            entries,
        };
        self.step(K14)?;
        // S6 commit
        if let Err(e) = self.step(K15) {
            if self.committed_on_error {
                self.apply_commit(rec, committed, ends, &manifest)?;
            }
            return Err(e);
        }
        self.apply_commit(rec, committed, ends, &manifest)?;
        self.step(K16)?;
        // S7 publish
        self.step(K17)?;
        self.d.journal = None;
        self.step(K18)?;
        self.step(K19)
    }

    fn unwind_to(&mut self, number: u64, blocks: Unwound) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        let (head_number, _) = self.head();
        let Some(old) = self.d.manifest() else {
            return Err(StoreError::Unwind(number));
        };
        let target = self.d.mdbx.canonical.get(&number).copied();
        let openable = target.is_some_and(|h| self.d.mdbx.state.at(h).is_ok());
        let (Some(target), true) = (target, openable) else {
            return Err(StoreError::Unwind(number));
        };
        if number >= head_number || number < old.entries[0].first_block {
            return Err(StoreError::Unwind(number));
        }
        // U1 journal
        self.write_journal(2, number, target, U1a, U1b)?;
        // U2 read-only: the new lengths and digests
        let mut ends = [(0u64, 0usize, 0usize); 3];
        for (i, e) in ends.iter_mut().enumerate() {
            *e = *self
                .d
                .mdbx
                .segidx
                .get(&(i, number))
                .ok_or(StoreError::Unwind(number))?;
        }
        let mut entries = old.entries;
        for (i, e) in entries.iter_mut().enumerate() {
            let (first, start, end) = ends[i];
            let digest = self.d.segs[i]
                .iter()
                .find(|f| f.first_block == first)
                .map_or(B256::ZERO, |f| keccak256(&f.bytes[start..end]));
            *e = SegmentEntry {
                last_block: number,
                len: idx(end),
                tail_digest: digest,
                ..*e
            };
        }
        let manifest = Manifest {
            generation: self.d.mdbx.generation + 1,
            head_number: number,
            head_hash: target,
            entries,
        };
        // U3 one transaction
        self.step(U3a)?;
        let m = &mut self.d.mdbx;
        if m.state.unwind_to(number).is_err() {
            return Err(StoreError::Unwind(number));
        }
        for n in number + 1..=head_number {
            if let Some(h) = m.canonical.remove(&n) {
                if blocks == Unwound::Drop {
                    m.records.remove(&h);
                }
            }
        }
        m.segidx.retain(|(_, b), _| *b <= number);
        if let Some(c) = m.ckpt.get_mut(&Stage::Finish) {
            *c = (*c).min(number); // Finish never stands above the head
        }
        m.generation += 1;
        m.manifest = Some(manifest.encode());
        self.step(U3b)?;
        // U4 truncate the segments
        for (i, &(first, _, end)) in ends.iter().enumerate() {
            let files = &mut self.d.segs[i];
            files.retain(|f| f.first_block <= first);
            if let Some(f) = files.last_mut() {
                f.bytes.truncate(end);
                f.synced = f.synced.min(end);
            }
            if i == 0 {
                self.step(U4a)?;
            }
        }
        self.step(U4b)?;
        // U5 journal
        self.d.journal = None;
        self.step(U5a)?;
        self.step(U5b)
    }

    fn put_side(&mut self, rec: &Record) -> Result<(), StoreError> {
        self.alive()?;
        for a in rec.diff.accounts.values().flatten() {
            a.balance.check_legal().map_err(CommitError::from)?;
        }
        self.step(KillPoint::K14)?;
        let m = &mut self.d.mdbx;
        m.stateless.remove(&rec.block.hash());
        m.records.insert(rec.block.hash(), rec.clone());
        self.step(KillPoint::K16)
    }

    fn put_stateless(&mut self, block: &Block) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        self.d.mdbx.stateless.insert(block.hash(), block.clone());
        self.step(KillPoint::K16)
    }

    fn checkpoint(&self, stage: Stage) -> u64 {
        match stage {
            Stage::Execution => self.head().0,
            s => self.d.mdbx.ckpt.get(&s).copied().unwrap_or(0),
        }
    }

    fn staged_header(&self, number: u64) -> Option<Header> {
        self.d.mdbx.staged_headers.get(&number).cloned()
    }

    fn staged_body(&self, number: u64) -> Option<Body> {
        self.d.mdbx.staged_bodies.get(&number).cloned()
    }

    fn stage_headers(&mut self, headers: &[Header]) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        let m = &mut self.d.mdbx;
        for h in headers {
            m.staged_headers.insert(h.number, h.clone());
        }
        if let Some(h) = headers.last() {
            m.ckpt.insert(Stage::Headers, h.number);
        }
        self.step(KillPoint::K16)
    }

    fn stage_bodies(&mut self, from: u64, bodies: &[Body]) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        let m = &mut self.d.mdbx;
        for (n, b) in (from..).zip(bodies) {
            m.staged_bodies.insert(n, b.clone());
        }
        if !bodies.is_empty() {
            m.ckpt.insert(Stage::Bodies, from + idx(bodies.len()) - 1);
        }
        self.step(KillPoint::K16)
    }

    fn set_checkpoint(&mut self, stage: Stage, number: u64) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        self.d.mdbx.ckpt.insert(stage, number);
        self.step(KillPoint::K16)
    }

    fn finish(&mut self, up_to: u64) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        let m = &mut self.d.mdbx;
        m.staged_headers = m.staged_headers.split_off(&(up_to + 1));
        m.staged_bodies = m.staged_bodies.split_off(&(up_to + 1));
        m.ckpt.insert(Stage::Finish, up_to);
        self.step(KillPoint::K16)
    }

    fn unwind_stages(&mut self, number: u64) -> Result<(), StoreError> {
        self.alive()?;
        self.step(KillPoint::K14)?;
        let m = &mut self.d.mdbx;
        let _ = m.staged_headers.split_off(&(number + 1));
        let _ = m.staged_bodies.split_off(&(number + 1));
        for c in m.ckpt.values_mut() {
            *c = (*c).min(number);
        }
        self.step(KillPoint::K16)
    }

    fn state_rows(&self, hash: B256) -> Result<StateRows, Missing> {
        let (accounts, storage) = self.d.mdbx.state.rows_at(hash)?;
        Ok(StateRows { accounts, storage })
    }

    fn anchor(&self) -> Option<Bytes> {
        self.d.mdbx.anchor.clone()
    }

    fn install_snapshot(&mut self, s: Install) -> Result<(), StoreError> {
        self.alive()?;
        let m = &self.d.mdbx;
        if m.generation != 0 || m.manifest.is_some() || !m.canonical.is_empty() {
            return Err(StoreError::NotEmpty);
        }
        let bad = StoreError::BadInstall;
        let (Some(pivot), Some(top)) = (s.records.last(), s.headers.last()) else {
            return Err(bad("no blocks"));
        };
        if pivot.block.hash() != top.hash() || s.headers.len() < s.records.len() {
            return Err(bad("the last header is not the last record"));
        }
        let linked = |h: &[Header]| {
            h.windows(2)
                .all(|w| w[1].number == w[0].number + 1 && w[1].parent_hash == w[0].hash())
        };
        if !linked(&s.headers)
            || !s.records.windows(2).all(|w| {
                w[1].block.number() == w[0].block.number() + 1
                    && w[1].block.parent() == w[0].block.hash()
            })
        {
            return Err(bad("blocks are not consecutive and linked"));
        }
        let (number, hash) = (pivot.block.number(), pivot.block.hash());
        let mut flat = MemFlat::default();
        for (a, x) in &s.state.accounts {
            x.balance.check_legal().map_err(CommitError::from)?;
            flat.set_account(*a, Some(x.clone()));
        }
        for (a, k, v) in &s.state.storage {
            flat.set_slot(*a, *k, *v);
        }
        if flat_root(&flat) != pivot.block.header.state_root {
            return Err(bad("the state does not give the pivot's state root"));
        }
        let first = |h: &Header| h.number;
        let (h_first, r_first) = (first(&s.headers[0]), pivot_first(&s.records));
        let mut segs: [Vec<File>; 3] = Default::default();
        let mut segidx = BTreeMap::new();
        let mut entries = [SegmentEntry {
            segment: Segment::Headers,
            first_block: 0,
            last_block: number,
            len: 0,
            tail_digest: B256::ZERO,
        }; 3];
        let mut put =
            |i: usize, n: u64, p: Vec<u8>, bytes: &mut Vec<u8>, last: &mut (usize, usize)| {
                let f = frame(n, &p);
                *last = (bytes.len(), bytes.len() + f.len());
                segidx.insert(
                    (i, n),
                    (if i == 0 { h_first } else { r_first }, last.0, last.1),
                );
                bytes.extend_from_slice(&f);
            };
        let mut files: [Vec<u8>; 3] = Default::default();
        let mut tails = [(0usize, 0usize); 3];
        for h in &s.headers {
            put(
                0,
                h.number,
                alloy_rlp::encode(h),
                &mut files[0],
                &mut tails[0],
            );
        }
        for r in &s.records {
            let p = payloads(r);
            put(
                1,
                r.block.number(),
                p[1].clone(),
                &mut files[1],
                &mut tails[1],
            );
            put(
                2,
                r.block.number(),
                p[2].clone(),
                &mut files[2],
                &mut tails[2],
            );
        }
        for (i, bytes) in files.into_iter().enumerate() {
            let first_block = if i == 0 { h_first } else { r_first };
            entries[i] = SegmentEntry {
                segment: SEGS[i],
                first_block,
                last_block: number,
                len: idx(bytes.len()),
                tail_digest: keccak256(&bytes[tails[i].0..tails[i].1]),
            };
            segs[i].push(File {
                first_block,
                synced: bytes.len(),
                bytes,
                in_dir: true,
            });
        }
        let manifest = Manifest {
            generation: 1,
            head_number: number,
            head_hash: hash,
            entries,
        };
        let m = &mut self.d.mdbx;
        m.generation = 1;
        m.manifest = Some(manifest.encode());
        m.canonical = s.headers.iter().map(|h| (h.number, h.hash())).collect();
        m.headers_only = s
            .headers
            .iter()
            .filter(|h| h.number < r_first)
            .map(|h| (h.hash(), h.clone()))
            .collect();
        m.records = s.records.into_iter().map(|r| (r.block.hash(), r)).collect();
        m.state = StateProvider::new(flat, number, hash, s.retention);
        m.segidx = segidx;
        m.anchor = (!s.anchor.is_empty()).then_some(s.anchor);
        self.d.segs = segs;
        Ok(())
    }

    fn prune(&mut self, headers_from: u64, records_from: u64) -> Result<(), StoreError> {
        self.alive()?;
        let (head, _) = self.head();
        let Some(mut man) = self.d.manifest() else {
            return Err(StoreError::Unwind(0));
        };
        if headers_from > records_from || records_from > head {
            return Err(StoreError::Prune {
                headers_from,
                records_from,
                head,
            });
        }
        let (h_from, r_from) = (
            headers_from.max(man.entries[0].first_block),
            records_from.max(man.entries[1].first_block),
        );
        let m = &mut self.d.mdbx;
        m.canonical.retain(|n, _| *n >= h_from);
        m.segidx
            .retain(|(i, n), _| *n >= if *i == 0 { h_from } else { r_from });
        let low: Vec<_> = m
            .records
            .iter()
            .filter(|(_, r)| r.block.number() < r_from)
            .map(|(h, r)| (*h, r.block.number(), r.block.header.clone()))
            .collect();
        for (h, n, header) in low {
            m.records.remove(&h);
            if n >= h_from {
                m.headers_only.insert(h, header);
            }
        }
        m.headers_only.retain(|_, h| h.number >= h_from);
        m.staged_headers = m.staged_headers.split_off(&h_from);
        m.staged_bodies = m.staged_bodies.split_off(&r_from);
        for (i, e) in man.entries.iter_mut().enumerate() {
            e.first_block = if i == 0 { h_from } else { r_from };
            // A file that holds only blocks below the floor is unlinked; one that straddles it stays.
            let files = &mut self.d.segs[i];
            while files.get(1).is_some_and(|f| f.first_block <= e.first_block) {
                files.remove(0);
            }
        }
        man.generation += 1;
        m.generation += 1;
        m.manifest = Some(man.encode());
        Ok(())
    }

    fn scan(&self) -> Scan {
        let m = &self.d.mdbx;
        let manifest = match &m.manifest {
            None => ManifestState::Missing,
            Some(b) => match Manifest::decode(b) {
                Ok(x) => ManifestState::Ok(Box::new(x)),
                Err(e) => ManifestState::Invalid(e),
            },
        };
        let mut tails = Vec::new();
        for (i, files) in self.d.segs.iter().enumerate() {
            let entry = match &manifest {
                ManifestState::Ok(x) => Some(x.entries[i]),
                _ => None,
            };
            let active = entry.and_then(|e| {
                let (first, start, _) = *m.segidx.get(&(i, e.last_block))?;
                files
                    .iter()
                    .find(|f| f.first_block == first)
                    .map(|f| (f, start, e))
            });
            tails.push(match active {
                Some((f, start, e)) => {
                    let len = len_of(e.len);
                    let ok = f.bytes.len() >= len
                        && start <= len
                        && keccak256(&f.bytes[start..len]) == e.tail_digest;
                    let (mut pos, mut ahead) = (len, 0);
                    while f.bytes.len() >= pos + FRAME_HEAD && ok {
                        let p = u32::from_le_bytes([
                            f.bytes[pos],
                            f.bytes[pos + 1],
                            f.bytes[pos + 2],
                            f.bytes[pos + 3],
                        ]);
                        let end = pos + FRAME_HEAD + len_of(u64::from(p));
                        if end > f.bytes.len() {
                            break;
                        }
                        (pos, ahead) = (end, ahead + 1);
                    }
                    SegmentTail {
                        segment: SEGS[i],
                        present: true,
                        file_len: idx(f.bytes.len()),
                        committed_tail_ok: ok,
                        ahead_frames: ahead,
                    }
                }
                None => SegmentTail {
                    segment: SEGS[i],
                    present: entry.is_none() && !files.is_empty(),
                    file_len: 0,
                    committed_tail_ok: false,
                    ahead_frames: 0,
                },
            });
        }
        Scan {
            generation: m.generation,
            manifest,
            tails,
            has_records: !m.canonical.is_empty()
                || !m.records.is_empty()
                || !m.stateless.is_empty(),
        }
    }

    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError> {
        self.alive()?;
        // Orphan chunk files (a crash between K09 and K16, or an unwind that cut below a chunk)
        // carry no committed data: they go first, so that the active file is the last one.
        if let Some(last) = self.d.manifest().map(|m| m.head_number) {
            for files in &mut self.d.segs {
                files.retain(|f| f.first_block <= last);
            }
        }
        for a in actions {
            let i = SEGS.iter().position(|s| *s == a.segment).unwrap_or(0);
            if let Some(f) = self.d.segs[i].last_mut() {
                let to = len_of(a.to_len);
                if a.quarantine {
                    self.d
                        .quarantine
                        .extend_from_slice(&f.bytes[to.min(f.bytes.len())..]);
                }
                f.bytes.truncate(to);
                f.synced = f.synced.min(to);
            }
        }
        // Idempotent publication: the journal is an input log, never replayed here.
        self.d.journal_tmp = None;
        self.d.journal = None;
        Ok(())
    }
}
