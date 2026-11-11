//! [`MdbxStore`]: the [`Store`] over MDBX tables and static-file segments, implementing ADR-005
//! exactly: the commit order of section 5 (journal, execute, stage segments, fsync segments, fsync
//! directories, one MDBX transaction that carries the manifest and the generation, commit, publish),
//! the restart reconciliation of section 7, the atomic unwind of section 8, the failure policy of
//! section 9 and pruning (section 10).
//!
//! MDBX is the authority for every table. Reads of state go through an in-memory
//! [`StateProvider`] that is rebuilt from the tables at start-up and checked against the head's
//! state root; it is a read model, never written ahead of the tables. Segment bytes beyond the
//! committed manifest length are staged and never read.
//!
//! ponytail: the flat state and the trie live in memory (the trie is xone-state's, and has no
//! tables yet). Replace [`StateProvider`] with a table-backed one when the trie is persisted; the
//! tables and the protocol here do not change.
//!
//! Kill points: [`KillPoint`] names every step; the store asks its segment directory whether one fires
//! (`SegDir::probe`). Only the in-memory disk of the fault harness ever answers yes.

mod codec;

use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use codec::{
    block_rlp, body_rlp, dec_account, dec_block, dec_body, dec_diff, dec_header, dec_link,
    dec_receipts, dec_record, enc_block, enc_diff, enc_link, enc_record, header_rlp, receipts_rlp,
};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_db::{
    mdbx::{Env, MdbxError, Rd, Wr, T},
    segments::{
        chunk_of, is_enospc, journal_bytes, SegDir, SegError, SegFile, Segs, Staged, JOURNAL,
        JOURNAL_COMMIT, JOURNAL_TMP, JOURNAL_UNWIND,
    },
    Action, Cause, DecodeError, Durability, Manifest, ManifestState, Segment, SegmentEntry,
    SegmentTail, GEN_KEY, MANIFEST_KEY,
};
use xone_primitives::Header;
use xone_state::{
    trie::flat_root, ChangeSet, CommitError, FlatRead, FlatWrite, MemFlat, Missing, StateDiff,
    StateProvider, StateRead,
};

use crate::store::{Block, KillPoint, Record, Scan, Stage, Store, StoreError, Unwound};

const COMMITTED_KEY: &[u8] = b"committed";
const FSYNC_FAILED: &str = "FSYNC_FAILED";

pub struct Config {
    /// Blocks above the oldest openable one that keep a changeset (the unwind and state window).
    pub retention: u64,
    /// Blocks per segment chunk file.
    pub per_file: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// ADR-005 section 7, P2: a sync failed earlier in this boot, so the page cache may hold bytes
    /// the device never got. Reopen after the next boot.
    #[error("an fsync failed earlier in this boot ({0}); refusing to open until the next boot")]
    FsyncFailedSameBoot(String),
    #[error("{0}")]
    Mdbx(#[from] MdbxError),
    #[error("{0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy)]
enum Dead {
    Crashed(KillPoint),
    Failed(Cause),
}

fn be(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}

fn seg_key(s: Segment, n: u64) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = s.id();
    k[1..].copy_from_slice(&be(n));
    k
}

fn le_u64(b: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(..8)?.try_into().ok()?))
}

fn be_u64(b: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

fn cs_key(n: u64, a: &Address) -> Vec<u8> {
    [&be(n)[..], a.as_slice()].concat()
}

fn cs_skey(n: u64, a: &Address, s: &B256) -> Vec<u8> {
    [&be(n)[..], a.as_slice(), s.as_slice()].concat()
}

fn hashed_skey(a: &Address, s: &B256) -> Vec<u8> {
    [keccak256(a).as_slice(), keccak256(s).as_slice()].concat()
}

type R<V> = Result<V, MdbxError>;

fn put_account(tx: &Wr, a: &Address, rlp: &[u8]) -> R<()> {
    tx.put(T::PlainAccounts, a.as_slice(), rlp)?;
    tx.put(T::HashedAccounts, keccak256(a).as_slice(), rlp)?;
    tx.put(T::Preimages, keccak256(a).as_slice(), a.as_slice())
}

fn del_account(tx: &Wr, a: &Address) -> R<()> {
    tx.del(T::PlainAccounts, a.as_slice())?;
    tx.del(T::HashedAccounts, keccak256(a).as_slice())
}

fn put_slot(tx: &Wr, a: &Address, s: &B256, v: &[u8]) -> R<()> {
    tx.put(T::PlainStorage, &[a.as_slice(), s.as_slice()].concat(), v)?;
    tx.put(T::HashedStorages, &hashed_skey(a, s), v)?;
    tx.put(T::Preimages, keccak256(a).as_slice(), a.as_slice())?;
    tx.put(T::Preimages, keccak256(s).as_slice(), s.as_slice())
}

fn del_slot(tx: &Wr, a: &Address, s: &B256) -> R<()> {
    tx.del(T::PlainStorage, &[a.as_slice(), s.as_slice()].concat())?;
    tx.del(T::HashedStorages, &hashed_skey(a, s))
}

/// Writes the diff of block `n` into the state tables and its changeset (whole prior records) into
/// the changeset tables: the same semantics as `StateDiff::apply`, over the tables.
fn write_diff(tx: &Wr, n: u64, d: &StateDiff) -> R<()> {
    for (a, new) in &d.accounts {
        let prior = match tx.get(T::PlainAccounts, a.as_slice())? {
            None => vec![0],
            Some(r) => [&[1][..], &r].concat(),
        };
        tx.put(T::AccountChangeSets, &cs_key(n, a), &prior)?;
        if new.is_none() {
            for (k, v) in tx.prefix(T::PlainStorage, a.as_slice())? {
                let slot = B256::from_slice(&k[20..]);
                tx.put(T::StorageChangeSets, &cs_skey(n, a, &slot), &v)?;
                del_slot(tx, a, &slot)?;
            }
        }
    }
    for ((a, s), v) in &d.storage {
        if matches!(d.accounts.get(a), Some(None)) {
            continue;
        }
        let key = [a.as_slice(), s.as_slice()].concat();
        let prior = tx
            .get(T::PlainStorage, &key)?
            .unwrap_or_else(|| vec![0; 32]);
        tx.put(T::StorageChangeSets, &cs_skey(n, a, s), &prior)?;
        if v.is_zero() {
            del_slot(tx, a, s)?;
        } else {
            put_slot(tx, a, s, &v.to_be_bytes::<32>())?;
        }
    }
    for (a, new) in &d.accounts {
        match new {
            Some(x) => put_account(tx, a, &x.rlp())?,
            None => del_account(tx, a)?,
        }
    }
    Ok(())
}

/// Writes the changeset rows of block `k` back into the state tables and deletes them.
fn revert_block(tx: &Wr, k: u64) -> R<()> {
    for (key, val) in tx.prefix(T::AccountChangeSets, &be(k))? {
        let a = Address::from_slice(&key[8..28]);
        match val.split_first() {
            Some((1, rlp)) => put_account(tx, &a, rlp)?,
            _ => del_account(tx, &a)?,
        }
        tx.del(T::AccountChangeSets, &key)?;
    }
    for (key, val) in tx.prefix(T::StorageChangeSets, &be(k))? {
        let (a, s) = (
            Address::from_slice(&key[8..28]),
            B256::from_slice(&key[28..60]),
        );
        if val.iter().all(|b| *b == 0) {
            del_slot(tx, &a, &s)?;
        } else {
            put_slot(tx, &a, &s, &val)?;
        }
        tx.del(T::StorageChangeSets, &key)?;
    }
    Ok(())
}

fn changeset_of(rd: &Rd, k: u64) -> Result<ChangeSet, StoreError> {
    let bad = || StoreError::Damaged(format!("changeset of block {k}"));
    let mut cs = ChangeSet::default();
    for (key, val) in rd.prefix(T::AccountChangeSets, &be(k)).map_err(|_| bad())? {
        let a = Address::from_slice(key.get(8..28).ok_or_else(bad)?);
        let acc = match val.split_first() {
            Some((1, rlp)) => Some(dec_account(rlp).ok_or_else(bad)?),
            _ => None,
        };
        cs.accounts.insert(a, acc);
    }
    for (key, val) in rd.prefix(T::StorageChangeSets, &be(k)).map_err(|_| bad())? {
        let (a, s) = (
            Address::from_slice(key.get(8..28).ok_or_else(bad)?),
            B256::from_slice(key.get(28..60).ok_or_else(bad)?),
        );
        cs.storage.insert((a, s), U256::from_be_slice(&val));
    }
    Ok(cs)
}

pub struct MdbxStore<D: SegDir> {
    env: Arc<Env>,
    segs: Segs<D>,
    cfg: Config,
    path: PathBuf,
    generation: u64,
    manifest: Option<Manifest>,
    head: (u64, B256),
    committed: Option<Link>,
    /// Oldest openable block: rows of blocks at or below it have no changeset.
    base: u64,
    state: StateProvider<MemFlat>,
    side: HashMap<B256, Record>,
    stateless: HashMap<B256, Block>,
    degraded: Option<Cause>,
    reconciling: bool,
    dead: Option<Dead>,
    /// A fired `Probe::IoCommitted`: the commit in flight is applied before the error is returned.
    committed_on_error: bool,
}

impl<D: SegDir> MdbxStore<D> {
    /// P2, P3: opens `chain.mdbx` at `path` over the segment directory `dir`. Nothing is checked
    /// beyond that: [`crate::chain::resume`] runs `scan`, `reconcile` and `repair`.
    pub fn open(path: &Path, dir: D, cfg: Config) -> Result<Self, OpenError> {
        if let Some(f) = dir.open(FSYNC_FAILED)? {
            let mut b = vec![0u8; usize::try_from(f.size()?).unwrap_or(0)];
            f.read_at(0, &mut b)?;
            let boot = dir.boot_id();
            if b == boot.as_bytes() {
                return Err(OpenError::FsyncFailedSameBoot(boot));
            }
        }
        let names: Vec<&str> = T::ALL.iter().map(|t| t.name()).collect();
        let env = Env::open(path, &names)?;
        let mut s = Self {
            env: Arc::new(env),
            segs: Segs::new(dir, cfg.per_file),
            state: StateProvider::new(MemFlat::default(), 0, B256::ZERO, cfg.retention),
            cfg,
            path: path.to_owned(),
            generation: 0,
            manifest: None,
            head: (0, B256::ZERO),
            committed: None,
            base: 0,
            side: HashMap::new(),
            stateless: HashMap::new(),
            degraded: None,
            reconciling: true,
            dead: None,
            committed_on_error: false,
        };
        let rd = s.env.read()?;
        s.generation = rd
            .get(T::Meta, GEN_KEY)?
            .and_then(|b| le_u64(&b))
            .unwrap_or(0);
        s.manifest = rd
            .get(T::Meta, MANIFEST_KEY)?
            .and_then(|b| Manifest::decode(&b).ok());
        s.committed = rd.get(T::Meta, COMMITTED_KEY)?.and_then(|b| dec_link(&b));
        drop(rd);
        if let Some(m) = &s.manifest {
            s.head = (m.head_number, m.head_hash);
            s.segs.publish(m);
        }
        Ok(s)
    }

    /// The directory and the MDBX path, after the store is dropped: what a restart opens.
    pub fn into_parts(self) -> (PathBuf, D) {
        (self.path, self.segs.dir)
    }

    pub fn segs(&self) -> &Segs<D> {
        &self.segs
    }

    fn rd(&self) -> Result<Rd<'_>, StoreError> {
        self.env
            .read()
            .map_err(|e| StoreError::Damaged(e.to_string()))
    }

    fn alive(&self) -> Result<(), StoreError> {
        match self.dead {
            None => Ok(()),
            Some(Dead::Crashed(k)) => Err(StoreError::Crashed(k)),
            Some(Dead::Failed(c)) => Err(StoreError::Degraded(c)),
        }
    }

    /// One kill point (ADR-005 sections 5, 7, 8 and 10). A real directory never fires; the in-memory
    /// disk of the fault harness does.
    fn step(&mut self, point: KillPoint) -> Result<(), StoreError> {
        use xone_db::segments::Probe;
        let Some(p) = self.segs.dir.probe(point.name()) else {
            return Ok(());
        };
        self.committed_on_error = p == Probe::IoCommitted;
        let (dead, e) = match p {
            Probe::Crash => (Dead::Crashed(point), StoreError::Crashed(point)),
            Probe::Io | Probe::IoCommitted => {
                (Dead::Failed(Cause::Io), StoreError::Degraded(Cause::Io))
            }
        };
        self.dead = Some(dead);
        Err(e)
    }

    /// ADR-005 section 9, fail-stop: no retry in this process. The marker holds the boot id.
    fn fail_stop(&mut self, cause: Cause) -> StoreError {
        eprintln!("xone-db: degraded cause={cause:?} (fail-stop)");
        self.dead = Some(Dead::Failed(cause));
        let boot = self.segs.dir.boot_id();
        let r = (|| -> io::Result<()> {
            let mut f = self.segs.dir.create(FSYNC_FAILED)?;
            f.write_at(0, boot.as_bytes())?;
            f.sync_all()?;
            self.segs.dir.sync(".")
        })();
        let _ = r; // best effort: the guard of P2 is absent if this failed (open question 7)
        StoreError::Degraded(cause)
    }

    /// Truncates the segments to the committed length and removes an orphan chunk (section 9, ENOSPC).
    fn cleanup(&mut self) -> Result<(), SegError> {
        let Some(m) = self.manifest.clone() else {
            return Ok(());
        };
        for e in &m.entries {
            let chunk = chunk_of(e.last_block, self.cfg.per_file);
            self.segs.truncate(e.segment, chunk, e.len)?;
            self.segs.sync_chunk(e.segment, chunk)?;
        }
        for (s, c) in self.segs.orphans(&m)? {
            self.segs.remove_chunk(s, c)?;
        }
        self.segs.sync_dirs()
    }

    /// A failed write (not a sync). ENOSPC with a clean cleanup is recoverable: the store reads on
    /// and tries again. Anything else, or a failed cleanup, is fail-stop.
    fn write_failed(&mut self, cause: Cause) -> StoreError {
        if cause == Cause::NoSpace {
            if self.cleanup().is_ok() {
                eprintln!("xone-db: degraded cause=no_space");
                self.degraded = Some(Cause::NoSpace);
                return StoreError::Degraded(Cause::NoSpace);
            }
            return self.fail_stop(Cause::Io);
        }
        self.fail_stop(cause)
    }

    fn seg_failed(&mut self, e: &SegError) -> StoreError {
        match e {
            SegError::LenMismatch { .. } => self.fail_stop(Cause::Io),
            _ => self.write_failed(e.cause()),
        }
    }

    fn io_failed(&mut self, e: &io::Error) -> StoreError {
        self.write_failed(if is_enospc(e) {
            Cause::NoSpace
        } else {
            Cause::Io
        })
    }

    fn db_failed(&mut self, e: &MdbxError) -> StoreError {
        self.write_failed(e.cause())
    }

    /// A sync, or the MDBX commit, returned an error: always fail-stop.
    fn sync_failed(&mut self) -> StoreError {
        self.fail_stop(Cause::FsyncFailed)
    }

    /// S1 / U1.
    fn write_journal(
        &mut self,
        kind: u8,
        number: u64,
        hash: B256,
        payload: &[u8],
        points: (KillPoint, Option<KillPoint>, KillPoint),
    ) -> Result<(), StoreError> {
        let bytes = journal_bytes(kind, self.generation + 1, number, hash, payload);
        let mut f = self
            .segs
            .dir
            .create(JOURNAL_TMP)
            .map_err(|e| self.io_failed(&e))?;
        f.write_at(0, &bytes).map_err(|e| self.io_failed(&e))?;
        self.step(points.0)?;
        f.sync_all().map_err(|_| self.sync_failed())?;
        if let Some(k) = points.1 {
            self.step(k)?;
        }
        self.segs
            .dir
            .rename(JOURNAL_TMP, JOURNAL)
            .map_err(|e| self.io_failed(&e))?;
        self.step(points.2)?;
        self.segs
            .dir
            .sync("journal")
            .map_err(|_| self.sync_failed())
    }

    /// S7 / U5. The head is already published: a failure here is fail-stop and the next start
    /// deletes the stale journal.
    fn remove_journal(&mut self, points: (KillPoint, KillPoint)) -> Result<(), StoreError> {
        self.segs
            .dir
            .remove(JOURNAL)
            .map_err(|_| self.fail_stop(Cause::Io))?;
        self.step(points.0)?;
        self.segs
            .dir
            .sync("journal")
            .map_err(|_| self.sync_failed())?;
        self.step(points.1)
    }

    // ---- reads

    fn seg_start(&self, rd: &Rd, s: Segment, n: u64) -> Option<u64> {
        le_u64(&rd.get(T::SegIdx, &seg_key(s, n)).ok()??)
    }

    fn read_seg(&self, rd: &Rd, s: Segment, n: u64) -> Result<Vec<u8>, StoreError> {
        let start = self
            .seg_start(rd, s, n)
            .ok_or_else(|| StoreError::Damaged(format!("no index row for {s:?} of block {n}")))?;
        self.segs
            .read(s, n, start)
            .map_err(|e| StoreError::Damaged(e.to_string()))
    }

    fn canonical_number(&self, rd: &Rd, hash: B256) -> Option<u64> {
        be_u64(&rd.get(T::HeaderNumbers, hash.as_slice()).ok()??)
    }

    fn header_at(&self, rd: &Rd, n: u64) -> Result<Header, StoreError> {
        dec_header(&self.read_seg(rd, Segment::Headers, n)?)
            .ok_or_else(|| StoreError::Damaged(format!("header of block {n}")))
    }

    /// Header, body and receipts of canonical block `n`, every frame CRC-checked.
    pub fn canonical_block(
        &self,
        n: u64,
    ) -> Result<Option<(Header, Body, Vec<Bytes>)>, StoreError> {
        let rd = self.rd()?;
        if rd
            .get(T::CanonicalHeaders, &be(n))
            .map_err(|e| StoreError::Damaged(e.to_string()))?
            .is_none()
        {
            return Ok(None);
        }
        let bad = |what| StoreError::Damaged(format!("{what} of block {n}"));
        Ok(Some((
            self.header_at(&rd, n)?,
            dec_body(&self.read_seg(&rd, Segment::Bodies, n)?).ok_or_else(|| bad("body"))?,
            dec_receipts(&self.read_seg(&rd, Segment::Receipts, n)?)
                .ok_or_else(|| bad("receipts"))?,
        )))
    }

    fn canonical_record(&self, rd: &Rd, n: u64) -> Result<Record, StoreError> {
        let (header, body, receipts) = {
            let bad = |what| StoreError::Damaged(format!("{what} of block {n}"));
            (
                self.header_at(rd, n)?,
                dec_body(&self.read_seg(rd, Segment::Bodies, n)?).ok_or_else(|| bad("body"))?,
                dec_receipts(&self.read_seg(rd, Segment::Receipts, n)?)
                    .ok_or_else(|| bad("receipts"))?,
            )
        };
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        // Outside the retained window a canonical block has no diff and no aux rows.
        let diff = match rd.get(T::BlockDiffs, &be(n)).map_err(db)? {
            Some(b) => dec_diff(&b).ok_or_else(|| bad_row("diff", n))?,
            None => StateDiff::default(),
        };
        let aux = rd.get(T::Aux, &be(n)).map_err(db)?.unwrap_or_else(Vec::new);
        Ok(Record {
            block: Block::new(header, body),
            receipts,
            diff,
            aux: Bytes::from(aux),
        })
    }

    /// The flat tables of the head, for a node that writes its own state checkpoints.
    pub fn head_flat(&self) -> &MemFlat {
        self.state.flat()
    }

    /// State root over the whole flat state (for tests: equals the head's `state_root`).
    pub fn flat_root(&self) -> B256 {
        flat_root(self.state.flat())
    }

    /// Numbers of the canonical index, ascending.
    pub fn canonical_numbers(&self) -> Vec<u64> {
        self.rd()
            .and_then(|rd| {
                rd.range(T::CanonicalHeaders, &[], None)
                    .map_err(|e| StoreError::Damaged(e.to_string()))
            })
            .map(|rows| rows.iter().filter_map(|(k, _)| be_u64(k)).collect())
            .unwrap_or_else(|_| Vec::new())
    }

    /// Hashes of the side blocks with state and of the stateless ones, sorted.
    pub fn side_hashes(&self) -> Vec<B256> {
        let mut v: Vec<B256> = self
            .side
            .keys()
            .chain(self.stateless.keys())
            .copied()
            .collect();
        v.sort();
        v
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// Contract code by hash (`Bytecodes`).
    pub fn code(&self, hash: B256) -> Option<Bytes> {
        let b = self.rd().ok()?.get(T::Bytecodes, hash.as_slice()).ok()??;
        Some(Bytes::from(b))
    }

    /// A consensus row (`ConsensusEpoch` or `ConsensusSnapshot`) of block `number`.
    pub fn consensus(&self, table: T, number: u64) -> Option<Vec<u8>> {
        self.rd().ok()?.get(table, &be(number)).ok()?
    }

    // ---- writes outside the head: one MDBX transaction each

    /// S5 and S6 for a write that does not move the head: the rows and `after` (the in-memory
    /// side) appear together or not at all.
    fn small_txn(
        &mut self,
        f: impl FnOnce(&Wr) -> R<()>,
        after: impl FnOnce(&mut Self),
    ) -> Result<(), StoreError> {
        self.alive()?;
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        f(&tx).map_err(|e| self.db_failed(&e))?;
        self.step(KillPoint::K14)?;
        tx.commit().map_err(|_| self.sync_failed())?;
        after(self);
        self.step(KillPoint::K16)
    }

    /// Stores contract code. Not a head move: no generation.
    pub fn put_code(&mut self, hash: B256, code: &[u8]) -> Result<(), StoreError> {
        self.small_txn(|tx| tx.put(T::Bytecodes, hash.as_slice(), code), |_| ())
    }

    /// Stores a consensus row of block `number`; an unwind below `number` deletes it.
    pub fn put_consensus(&mut self, table: T, number: u64, row: &[u8]) -> Result<(), StoreError> {
        self.small_txn(|tx| tx.put(table, &be(number), row), |_| ())
    }

    // ---- the commit protocol

    fn next_manifest(
        &self,
        number: u64,
        hash: B256,
        ends: [(u64, B256); 3],
        first_block: u64,
    ) -> Manifest {
        let mut entries = [SegmentEntry {
            segment: Segment::Headers,
            first_block,
            last_block: number,
            len: 0,
            tail_digest: B256::ZERO,
        }; 3];
        for (i, s) in Segment::ALL.into_iter().enumerate() {
            entries[i] = SegmentEntry {
                segment: s,
                first_block: self
                    .manifest
                    .as_ref()
                    .map_or(first_block, |m| m.entries[i].first_block),
                last_block: number,
                len: ends[i].0,
                tail_digest: ends[i].1,
            };
        }
        Manifest {
            generation: self.generation + 1,
            head_number: number,
            head_hash: hash,
            entries,
        }
    }

    fn meta_rows(tx: &Wr, m: &Manifest) -> R<()> {
        tx.put(T::Meta, GEN_KEY, &m.generation.to_le_bytes())?;
        tx.put(T::Meta, MANIFEST_KEY, &m.encode())
    }

    /// The rows of one block that extends the head (S5). `anchor` is the whole starting state of a
    /// new datadir; otherwise the block's diff and its changeset are written.
    #[allow(clippy::too_many_arguments)]
    fn commit_rows(
        &self,
        tx: &Wr,
        rec: &Record,
        staged: &[Staged; 3],
        m: &Manifest,
        committed: Option<&Link>,
        anchor: Option<&MemFlat>,
        new_base: u64,
    ) -> R<()> {
        let b = &rec.block;
        let n = b.number();
        for (i, s) in Segment::ALL.into_iter().enumerate() {
            tx.put(T::SegIdx, &seg_key(s, n), &staged[i].start.to_le_bytes())?;
        }
        tx.put(T::CanonicalHeaders, &be(n), b.hash().as_slice())?;
        tx.put(T::HeaderNumbers, b.hash().as_slice(), &be(n))?;
        tx.del(T::SideBlocks, b.hash().as_slice())?;
        tx.del(T::Stateless, b.hash().as_slice())?;
        match anchor {
            Some(flat) => {
                for (a, acc) in flat.accounts() {
                    put_account(tx, &a, &acc.rlp())?;
                    for (s, v) in flat.slots(a) {
                        put_slot(tx, &a, &s, &v.to_be_bytes::<32>())?;
                    }
                }
            }
            None => {
                write_diff(tx, n, &rec.diff)?;
                tx.put(T::BlockDiffs, &be(n), &enc_diff(&rec.diff))?;
                if !rec.aux.is_empty() {
                    tx.put(T::Aux, &be(n), &rec.aux)?;
                }
                if new_base > self.base {
                    // Aux rows stay: executors restore their context from every canonical block.
                    let upto = be(new_base + 1);
                    for t in [T::BlockDiffs, T::AccountChangeSets, T::StorageChangeSets] {
                        tx.del_range(t, &[], Some(&upto))?;
                    }
                }
            }
        }
        if let Some(l) = committed {
            tx.put(T::Meta, COMMITTED_KEY, &enc_link(l))?;
        }
        Self::meta_rows(tx, m)
    }

    /// S1 to S7 for one block. `anchor`: the first commit of a new datadir.
    fn commit_inner(
        &mut self,
        rec: &Record,
        committed: Option<Link>,
        anchor: Option<MemFlat>,
    ) -> Result<(), StoreError> {
        use KillPoint::*;
        let b = &rec.block;
        let n = b.number();
        // S1 journal
        self.step(K01)?;
        self.write_journal(
            JOURNAL_COMMIT,
            n,
            b.hash(),
            &block_rlp(b),
            (K02, Some(K03), K04),
        )?;
        // S2 execute: nothing durable
        self.step(K05)?;
        // S3 stage
        let payloads = [
            header_rlp(&b.header),
            body_rlp(&b.body),
            receipts_rlp(&rec.receipts),
        ];
        let mut staged = [None; 3];
        for (i, s) in Segment::ALL.into_iter().enumerate() {
            let st = self
                .segs
                .stage(s, n, &payloads[i])
                .map_err(|e| self.seg_failed(&e))?;
            staged[i] = Some(st);
            self.step([K06, K07, K08][i])?;
        }
        let staged = staged.map(|s| {
            s.unwrap_or(Staged {
                start: 0,
                end: 0,
                digest: B256::ZERO,
                created: false,
            })
        });
        if staged.iter().any(|s| s.created) {
            self.step(K09)?;
        }
        // S4 sync
        for (i, s) in Segment::ALL.into_iter().enumerate() {
            self.segs.sync_file(s, n).map_err(|_| self.sync_failed())?;
            self.step([K10, K11, K12][i])?;
        }
        self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
        self.step(K13)?;
        // S5 transaction
        let first = self
            .manifest
            .as_ref()
            .map_or(n, |m| m.entries[0].first_block);
        let manifest = self.next_manifest(
            n,
            b.hash(),
            [
                (staged[0].end, staged[0].digest),
                (staged[1].end, staged[1].digest),
                (staged[2].end, staged[2].digest),
            ],
            first,
        );
        let new_base = if anchor.is_some() {
            n
        } else {
            self.base.max(n.saturating_sub(self.cfg.retention))
        };
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        self.commit_rows(
            &tx,
            rec,
            &staged,
            &manifest,
            committed.as_ref(),
            anchor.as_ref(),
            new_base,
        )
        .map_err(|e| self.db_failed(&e))?;
        self.step(K14)?;
        // S6 commit
        if let Err(e) = self.step(K15) {
            if self.committed_on_error && tx.commit().is_ok() {
                // the commit is applied, then the call reports an error: the outcome is unknown
                let _ = self.publish_commit(rec, &manifest, committed, anchor.as_ref(), new_base);
            }
            return Err(e);
        }
        tx.commit().map_err(|_| self.sync_failed())?;
        self.degraded = None;
        self.publish_commit(rec, &manifest, committed, anchor.as_ref(), new_base)?;
        self.step(K16)?;
        // S7 publish: the visible lengths swap, then the journal goes
        self.segs.publish(&manifest);
        self.step(K17)?;
        self.remove_journal((K18, K19))
    }

    /// The commit returned: the generation, the head and the read model move together.
    fn publish_commit(
        &mut self,
        rec: &Record,
        m: &Manifest,
        committed: Option<Link>,
        anchor: Option<&MemFlat>,
        new_base: u64,
    ) -> Result<(), StoreError> {
        let b = &rec.block;
        self.generation = m.generation;
        self.manifest = Some(m.clone());
        self.head = (b.number(), b.hash());
        if committed.is_some() {
            self.committed = committed;
        }
        self.side.remove(&b.hash());
        self.stateless.remove(&b.hash());
        self.base = new_base;
        match anchor {
            Some(f) => {
                self.state =
                    StateProvider::new(f.clone(), b.number(), b.hash(), self.cfg.retention);
            }
            None => {
                if let Err(e) = self.state.commit_block(b.hash(), b.parent(), &rec.diff) {
                    // The tables hold the block and the model could not take it: stop, a restart reloads.
                    self.fail_stop(Cause::Io);
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    /// The first commit of a new datadir: `header` is the genesis block (or a trusted starting block)
    /// and `flat` its state. It goes through the same protocol as every block. A kill before it
    /// returns leaves files without an MDBX authority (row D2): the next start refuses, and an operator
    /// removes the datadir, which holds nothing.
    pub fn init_anchor(&mut self, header: Header, flat: MemFlat) -> Result<(), StoreError> {
        self.alive()?;
        if self.manifest.is_some() || self.generation != 0 {
            return Err(StoreError::Damaged("the datadir is not new".into()));
        }
        let rec = Record {
            block: Block::new(header, Body::default()),
            receipts: Vec::new(),
            diff: StateDiff::default(),
            aux: Bytes::new(),
        };
        self.commit_inner(&rec, None, Some(flat))?;
        self.reconciling = false;
        Ok(())
    }

    // ---- start-up

    fn load_state(&mut self) -> Result<(), StoreError> {
        let Some(m) = self.manifest.clone() else {
            return Ok(());
        };
        let (head_n, head_h) = (m.head_number, m.head_hash);
        let bad = |what: &str| StoreError::Damaged(format!("state tables: {what}"));
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.rd()?;
        let mut flat = MemFlat::default();
        for (k, v) in rd.range(T::PlainAccounts, &[], None).map_err(db)? {
            flat.set_account(
                Address::from_slice(k.get(..20).ok_or_else(|| bad("account key"))?),
                Some(dec_account(&v).ok_or_else(|| bad("account row"))?),
            );
        }
        for (k, v) in rd.range(T::PlainStorage, &[], None).map_err(db)? {
            flat.set_slot(
                Address::from_slice(k.get(..20).ok_or_else(|| bad("slot key"))?),
                B256::from_slice(k.get(20..52).ok_or_else(|| bad("slot key"))?),
                U256::from_be_slice(&v),
            );
        }
        let head_header = self.header_at(&rd, head_n)?;
        if head_header.hash() != head_h || flat_root(&flat) != head_header.state_root {
            return Err(bad("the flat state does not give the head's state root"));
        }
        let base = match rd.first(T::BlockDiffs).map_err(db)? {
            Some((k, _)) => be_u64(&k).ok_or_else(|| bad("diff key"))?.saturating_sub(1),
            None => head_n,
        };
        let mut hashes = BTreeMap::new();
        for (k, v) in rd.range(T::CanonicalHeaders, &be(base), None).map_err(db)? {
            hashes.insert(
                be_u64(&k).ok_or_else(|| bad("index key"))?,
                B256::from_slice(&v),
            );
        }
        for k in (base + 1..=head_n).rev() {
            changeset_of(&rd, k)?.revert(&mut flat);
        }
        let base_hash = *hashes
            .get(&base)
            .ok_or_else(|| bad("no canonical hash at the base"))?;
        let mut state = StateProvider::new(flat, base, base_hash, self.cfg.retention);
        for k in base + 1..=head_n {
            let diff = rd
                .get(T::BlockDiffs, &be(k))
                .map_err(db)?
                .and_then(|b| dec_diff(&b))
                .ok_or_else(|| bad("a block diff"))?;
            let (hash, parent) = (hashes.get(&k), hashes.get(&(k - 1)));
            let (Some(hash), Some(parent)) = (hash, parent) else {
                return Err(bad("a hole in the canonical index"));
            };
            state
                .commit_block(*hash, *parent, &diff)
                .map_err(StoreError::State)?;
        }
        let mut side = HashMap::new();
        for (_, v) in rd.range(T::SideBlocks, &[], None).map_err(db)? {
            let r = dec_record(&v).ok_or_else(|| bad("a side block"))?;
            side.insert(r.block.hash(), r);
        }
        let mut stateless = HashMap::new();
        for (_, v) in rd.range(T::Stateless, &[], None).map_err(db)? {
            let b = dec_block(&v).ok_or_else(|| bad("a stateless block"))?;
            stateless.insert(b.hash(), b);
        }
        drop(rd);
        (self.state, self.base, self.side, self.stateless) = (state, base, side, stateless);
        Ok(())
    }

    fn has_records(&self) -> bool {
        let Ok(rd) = self.rd() else { return true };
        [T::CanonicalHeaders, T::SideBlocks, T::Stateless, T::SegIdx]
            .iter()
            .any(|t| !rd.is_empty(*t).unwrap_or(true))
    }

    // ---- pruning (ADR-005 section 10)

    /// Removes whole chunk files below `floor`, a multiple of the chunk size, in the three steps of
    /// section 10. Readers stop seeing the blocks at the commit. `floor` must not be above the oldest
    /// openable block: pruning never takes a block whose state or changeset is still served.
    pub fn prune_to(&mut self, floor: u64) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        let Some(old) = self.manifest.clone() else {
            return Err(StoreError::Unwind(floor));
        };
        if !floor.is_multiple_of(self.cfg.per_file)
            || floor <= old.entries[0].first_block
            || floor > self.base
        {
            return Err(StoreError::Unwind(floor));
        }
        self.step(PR1)?;
        let mut m = old;
        m.generation += 1;
        for e in &mut m.entries {
            e.first_block = floor;
        }
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        let r = (|| -> R<()> {
            let upto = be(floor);
            for (k, v) in tx.range(T::CanonicalHeaders, &[], Some(&upto))? {
                tx.del(T::HeaderNumbers, &v)?;
                tx.del(T::CanonicalHeaders, &k)?;
            }
            for s in Segment::ALL {
                tx.del_range(T::SegIdx, &seg_key(s, 0), Some(&seg_key(s, floor)))?;
            }
            for t in [
                T::BlockDiffs,
                T::Aux,
                T::AccountChangeSets,
                T::StorageChangeSets,
                T::ConsensusEpoch,
                T::ConsensusSnapshot,
            ] {
                tx.del_range(t, &[], Some(&upto))?;
            }
            Self::meta_rows(&tx, &m)
        })();
        r.map_err(|e| self.db_failed(&e))?;
        tx.commit().map_err(|_| self.sync_failed())?;
        self.generation = m.generation;
        self.manifest = Some(m.clone());
        self.step(PR2)?;
        self.unlink_below(&m, true)
    }

    /// P-2 and P-3: unlinks chunk files wholly below the first retained block, then syncs.
    fn unlink_below(&mut self, m: &Manifest, points: bool) -> Result<(), StoreError> {
        let mut first = true;
        let doomed: Vec<_> = self
            .segs
            .orphans(m)
            .map_err(|e| self.seg_failed(&e))?
            .into_iter()
            .filter(|(_, c)| *c < chunk_of(m.head_number, self.cfg.per_file))
            .collect();
        for (s, c) in doomed {
            self.segs
                .remove_chunk(s, c)
                .map_err(|e| self.seg_failed(&e))?;
            if points && first {
                self.step(KillPoint::PR3)?;
            }
            first = false;
        }
        if points {
            self.step(KillPoint::PR4)?;
        }
        self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
        if points {
            self.step(KillPoint::PR5)?;
        }
        Ok(())
    }
}

fn bad_row(what: &str, n: u64) -> StoreError {
    StoreError::Damaged(format!("{what} row of block {n}"))
}

impl<D: SegDir> MdbxStore<D> {
    fn unwind_inner(&mut self, number: u64, blocks: Unwound) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        let head_n = self.head.0;
        let Some(old) = self.manifest.clone() else {
            return Err(StoreError::Unwind(number));
        };
        let target = self.canonical_hash(number);
        let openable = target.is_some_and(|h| self.state.at(h).is_ok());
        let (Some(target), true) = (target, openable) else {
            return Err(StoreError::Unwind(number));
        };
        if number >= head_n || number < old.entries[0].first_block {
            return Err(StoreError::Unwind(number));
        }
        // U1 journal
        self.write_journal(JOURNAL_UNWIND, number, target, &[], (U1a, None, U1b))?;
        // U2 read-only: the new lengths and tail digests, and the blocks that stay as side blocks
        let (ends, dropped) = {
            let rd = self.rd()?;
            let mut ends = [(0u64, B256::ZERO); 3];
            for (i, s) in Segment::ALL.into_iter().enumerate() {
                let start = self
                    .seg_start(&rd, s, number)
                    .ok_or(StoreError::Unwind(number))?;
                let fr = xone_db::segments::frame(number, &self.read_seg(&rd, s, number)?);
                ends[i] = (
                    start + u64::try_from(fr.len()).unwrap_or(u64::MAX),
                    keccak256(&fr),
                );
            }
            let mut dropped = Vec::new();
            for k in (number + 1..=head_n).rev() {
                dropped.push(self.canonical_record(&rd, k)?);
            }
            (ends, dropped)
        };
        let mut manifest = old;
        manifest.generation += 1;
        manifest.head_number = number;
        manifest.head_hash = target;
        for (i, e) in manifest.entries.iter_mut().enumerate() {
            (e.last_block, e.len, e.tail_digest) = (number, ends[i].0, ends[i].1);
        }
        // U3 one transaction
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        let r = (|| -> R<()> {
            let above = be(number + 1);
            for rec in &dropped {
                let k = rec.block.number();
                revert_block(&tx, k)?;
                tx.del(T::HeaderNumbers, rec.block.hash().as_slice())?;
                if blocks == Unwound::Keep {
                    tx.put(T::SideBlocks, rec.block.hash().as_slice(), &enc_record(rec))?;
                }
            }
            for t in [
                T::CanonicalHeaders,
                T::BlockDiffs,
                T::Aux,
                T::ConsensusEpoch,
                T::ConsensusSnapshot,
                T::StagedHeaders,
                T::StagedBodies,
            ] {
                tx.del_range(t, &above, None)?;
            }
            for s in Segment::ALL {
                tx.del_range(T::SegIdx, &seg_key(s, number + 1), Some(&[s.id() + 1]))?;
            }
            if let Some(c) = tx.get(T::Stages, Stage::Finish.name().as_bytes())? {
                if be_u64(&c).is_some_and(|c| c > number) {
                    tx.put(T::Stages, Stage::Finish.name().as_bytes(), &be(number))?;
                }
            }
            Self::meta_rows(&tx, &manifest)
        })();
        r.map_err(|e| self.db_failed(&e))?;
        self.step(U3a)?;
        tx.commit().map_err(|_| self.sync_failed())?;
        self.degraded = None;
        self.generation = manifest.generation;
        self.head = (number, target);
        self.manifest = Some(manifest.clone());
        self.segs.publish(&manifest);
        if self.state.unwind_to(number).is_err() {
            return Err(self.fail_stop(Cause::Io));
        }
        if blocks == Unwound::Keep {
            for rec in dropped {
                self.side.insert(rec.block.hash(), rec);
            }
        }
        self.step(U3b)?;
        // U4 cut the segments
        let chunk = chunk_of(number, self.cfg.per_file);
        for (i, s) in Segment::ALL.into_iter().enumerate() {
            let cut = (|| -> Result<(), SegError> {
                self.segs.truncate(s, chunk, ends[i].0)?;
                for c in self.segs.chunks(s)? {
                    if c > chunk {
                        self.segs.remove_chunk(s, c)?;
                    }
                }
                Ok(())
            })();
            cut.map_err(|e| self.seg_failed(&e))?;
            self.segs
                .sync_chunk(s, chunk)
                .map_err(|_| self.sync_failed())?;
            if i == 0 {
                self.step(U4a)?;
            }
        }
        self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
        self.step(U4b)?;
        // U5 journal
        self.remove_journal((U5a, U5b))
    }

    fn stage_row(&self, t: T, number: u64) -> Option<Vec<u8>> {
        self.rd().ok()?.get(t, &be(number)).ok()?
    }
}

impl<D: SegDir> Store for MdbxStore<D> {
    fn head(&self) -> (u64, B256) {
        self.head
    }

    fn canonical_hash(&self, number: u64) -> Option<B256> {
        let b = self
            .rd()
            .ok()?
            .get(T::CanonicalHeaders, &be(number))
            .ok()??;
        (b.len() == 32).then(|| B256::from_slice(&b))
    }

    fn header(&self, hash: B256) -> Option<Header> {
        if let Some(r) = self.side.get(&hash) {
            return Some(r.block.header.clone());
        }
        if let Some(b) = self.stateless.get(&hash) {
            return Some(b.header.clone());
        }
        let rd = self.rd().ok()?;
        let n = self.canonical_number(&rd, hash)?;
        self.header_at(&rd, n).ok()
    }

    fn record(&self, hash: B256) -> Option<Record> {
        if let Some(r) = self.side.get(&hash) {
            return Some(r.clone());
        }
        let rd = self.rd().ok()?;
        let n = self.canonical_number(&rd, hash)?;
        self.canonical_record(&rd, n).ok()
    }

    fn stateless(&self, hash: B256) -> Option<Block> {
        self.stateless.get(&hash).cloned()
    }

    fn state_at(&self, hash: B256) -> Result<Box<dyn StateRead + '_>, Missing> {
        let mut diffs: Vec<&StateDiff> = Vec::new();
        let mut cur = hash;
        loop {
            if let Ok(view) = self.state.at(cur) {
                return Ok(Box::new(diffs.iter().rev().fold(view, |v, d| v.child(d))));
            }
            // A canonical block outside the retained window has no state: it is not a side block.
            let r = self.side.get(&cur).ok_or(Missing::Block(hash))?;
            diffs.push(&r.diff);
            cur = r.block.parent();
        }
    }

    fn committed(&self) -> Option<Link> {
        self.committed
    }

    fn stateless_children(&self, parent: B256) -> usize {
        self.stateless
            .values()
            .filter(|b| b.parent() == parent)
            .count()
    }

    fn durability(&self) -> Durability {
        match (self.dead, self.degraded, self.reconciling) {
            (Some(Dead::Failed(c)), _, _) => Durability::Degraded(c),
            (Some(Dead::Crashed(_)), _, _) => Durability::Degraded(Cause::Io),
            (None, Some(c), _) => Durability::Degraded(c),
            (None, None, true) => Durability::Degraded(Cause::Reconciling),
            (None, None, false) => Durability::Durable,
        }
    }

    fn commit_block(&mut self, rec: &Record, committed: Option<Link>) -> Result<(), StoreError> {
        self.alive()?;
        let b = &rec.block;
        if b.parent() != self.head.1 || b.number() != self.head.0 + 1 {
            return Err(CommitError::NotExtending {
                head: self.head.1,
                parent: b.parent(),
            }
            .into());
        }
        for a in rec.diff.accounts.values().flatten() {
            a.balance.check_legal().map_err(CommitError::from)?;
        }
        if self.manifest.is_none() {
            return Err(StoreError::Unwind(self.head.0));
        }
        self.commit_inner(rec, committed, None)
    }

    fn unwind_to(&mut self, number: u64, blocks: Unwound) -> Result<(), StoreError> {
        self.unwind_inner(number, blocks)
    }

    fn put_side(&mut self, rec: &Record) -> Result<(), StoreError> {
        self.alive()?;
        for a in rec.diff.accounts.values().flatten() {
            a.balance.check_legal().map_err(CommitError::from)?;
        }
        let (hash, enc) = (rec.block.hash(), enc_record(rec));
        self.small_txn(
            |tx| {
                tx.put(T::SideBlocks, hash.as_slice(), &enc)?;
                tx.del(T::Stateless, hash.as_slice())
            },
            |s| {
                s.stateless.remove(&hash);
                s.side.insert(hash, rec.clone());
            },
        )
    }

    fn put_stateless(&mut self, block: &Block) -> Result<(), StoreError> {
        let (hash, enc) = (block.hash(), enc_block(block));
        self.small_txn(
            |tx| tx.put(T::Stateless, hash.as_slice(), &enc),
            |s| {
                s.stateless.insert(hash, block.clone());
            },
        )
    }

    fn checkpoint(&self, stage: Stage) -> u64 {
        match stage {
            Stage::Execution => self.head.0,
            s => self
                .rd()
                .ok()
                .and_then(|rd| rd.get(T::Stages, s.name().as_bytes()).ok()?)
                .and_then(|b| be_u64(&b))
                .unwrap_or(0),
        }
    }

    fn staged_header(&self, number: u64) -> Option<Header> {
        dec_header(&self.stage_row(T::StagedHeaders, number)?)
    }

    fn staged_body(&self, number: u64) -> Option<Body> {
        dec_body(&self.stage_row(T::StagedBodies, number)?)
    }

    fn stage_headers(&mut self, headers: &[Header]) -> Result<(), StoreError> {
        self.small_txn(
            |tx| {
                for h in headers {
                    tx.put(T::StagedHeaders, &be(h.number), &header_rlp(h))?;
                }
                match headers.last() {
                    Some(h) => tx.put(T::Stages, Stage::Headers.name().as_bytes(), &be(h.number)),
                    None => Ok(()),
                }
            },
            |_| (),
        )
    }

    fn stage_bodies(&mut self, from: u64, bodies: &[Body]) -> Result<(), StoreError> {
        self.small_txn(
            |tx| {
                for (n, b) in (from..).zip(bodies) {
                    tx.put(T::StagedBodies, &be(n), &body_rlp(b))?;
                }
                match u64::try_from(bodies.len()) {
                    Ok(len) if len > 0 => tx.put(
                        T::Stages,
                        Stage::Bodies.name().as_bytes(),
                        &be(from + len - 1),
                    ),
                    _ => Ok(()),
                }
            },
            |_| (),
        )
    }

    fn set_checkpoint(&mut self, stage: Stage, number: u64) -> Result<(), StoreError> {
        self.small_txn(
            |tx| tx.put(T::Stages, stage.name().as_bytes(), &be(number)),
            |_| (),
        )
    }

    fn finish(&mut self, up_to: u64) -> Result<(), StoreError> {
        self.small_txn(
            |tx| {
                let below = be(up_to + 1);
                tx.del_range(T::StagedHeaders, &[], Some(&below))?;
                tx.del_range(T::StagedBodies, &[], Some(&below))?;
                tx.put(T::Stages, Stage::Finish.name().as_bytes(), &be(up_to))
            },
            |_| (),
        )
    }

    fn unwind_stages(&mut self, number: u64) -> Result<(), StoreError> {
        self.small_txn(
            |tx| {
                let above = be(number + 1);
                tx.del_range(T::StagedHeaders, &above, None)?;
                tx.del_range(T::StagedBodies, &above, None)?;
                for (k, v) in tx.range(T::Stages, &[], None)? {
                    if be_u64(&v).is_some_and(|c| c > number) {
                        tx.put(T::Stages, &k, &be(number))?;
                    }
                }
                Ok(())
            },
            |_| (),
        )
    }

    fn scan(&self) -> Scan {
        let rd = self.rd();
        let raw = rd
            .as_ref()
            .ok()
            .and_then(|rd| rd.get(T::Meta, MANIFEST_KEY).ok());
        let mut manifest = match raw {
            None => ManifestState::Invalid(DecodeError::BadLayout),
            Some(None) => ManifestState::Missing,
            Some(Some(b)) => match Manifest::decode(&b) {
                Ok(m) => ManifestState::Ok(Box::new(m)),
                Err(e) => ManifestState::Invalid(e),
            },
        };
        let tails = match &rd {
            Ok(rd) => self.segs.scan(&manifest, |s, n| self.seg_start(rd, s, n)),
            Err(_) => Ok(xone_db::segments::Scanned {
                tails: Segment::ALL
                    .iter()
                    .map(|s| SegmentTail {
                        segment: *s,
                        present: false,
                        file_len: 0,
                        committed_tail_ok: false,
                        ahead_frames: 0,
                    })
                    .collect(),
                header_version: None,
            }),
        };
        let tails = match tails {
            Ok(t) => t,
            // A read error at start-up is a refusal, not an empty datadir.
            Err(_) => {
                manifest = ManifestState::Invalid(DecodeError::BadLayout);
                xone_db::segments::Scanned {
                    tails: Vec::new(),
                    header_version: None,
                }
            }
        };
        if let (ManifestState::Ok(_), Some(v)) = (&manifest, tails.header_version) {
            manifest = ManifestState::Invalid(DecodeError::Version(v));
        }
        Scan {
            generation: self.generation,
            manifest,
            tails: tails.tails,
            has_records: self.has_records(),
        }
    }

    fn repair(&mut self, actions: &[Action]) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        if let Some(m) = self.manifest.clone() {
            // Orphan chunk files (a kill between K09 and K16, an unwind below a chunk, or a prune that
            // stopped before its unlinks) carry no committed data.
            let orphans = self.segs.orphans(&m).map_err(|e| self.seg_failed(&e))?;
            for (s, c) in &orphans {
                self.segs
                    .remove_chunk(*s, *c)
                    .map_err(|e| self.seg_failed(&e))?;
            }
            for a in actions {
                let e = xone_db::segments::entry(&m, a.segment);
                let chunk = chunk_of(e.last_block, self.cfg.per_file);
                if a.quarantine {
                    self.segs
                        .quarantine(a.segment, chunk, a.to_len, &format!("g{}", m.generation))
                        .map_err(|e| self.seg_failed(&e))?;
                    self.step(RP3)?;
                }
                self.segs
                    .truncate(a.segment, chunk, a.to_len)
                    .map_err(|e| self.seg_failed(&e))?;
                self.step(RP1)?;
                self.segs
                    .sync_chunk(a.segment, chunk)
                    .map_err(|_| self.sync_failed())?;
                self.step(RP2)?;
            }
            if !orphans.is_empty() || !actions.is_empty() {
                self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
            }
        }
        // Idempotent publication: the journal is an input log, never replayed here.
        self.segs
            .dir
            .remove(JOURNAL_TMP)
            .and_then(|()| self.segs.dir.remove(JOURNAL))
            .and_then(|()| self.segs.dir.sync("journal"))
            .map_err(|_| self.sync_failed())?;
        self.load_state()?;
        self.reconciling = false;
        Ok(())
    }
}
