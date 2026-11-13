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
pub mod diskflat;

use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_primitives::{keccak256, Bytes, B256, U256};
use codec::{
    block_rlp, body_rlp, dec_account, dec_block, dec_body, dec_diff, dec_header, dec_link,
    dec_receipts, dec_record, enc_block, enc_diff, enc_link, enc_record, header_rlp, receipts_rlp,
};
use diskflat::{DiskFlat, Flat};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_db::{
    mdbx::{Env, MdbxError, Rd, Wr, T},
    segments::{
        chunk_of, frame, is_enospc, journal_bytes, SegDir, SegError, SegFile, Segs, Staged,
        HEADER_LEN, JOURNAL, JOURNAL_COMMIT, JOURNAL_TMP, JOURNAL_UNWIND,
    },
    Action, Cause, DecodeError, Durability, Manifest, ManifestState, Segment, SegmentEntry,
    SegmentTail, GEN_KEY, MANIFEST_KEY,
};
use xone_primitives::Header;
use xone_state::{
    trie::{flat_root, root_of_sorted_rows},
    ChangeSet, CommitError, FlatRead, FlatWrite, MemFlat, Missing, StateDiff, StateProvider,
    StateRead,
};

use crate::store::{
    Block, Install, KillPoint, Record, Scan, Stage, StateRows, Store, StoreError, Unwound,
};

const COMMITTED_KEY: &[u8] = b"committed";
const ANCHOR_KEY: &[u8] = b"anchor";
/// Written with the first commit of a datadir whose every block commit writes its `TxLookup` rows (and
/// whose node writes its consensus rows). A datadir without it was made by an older build.
const HISTORY_KEY: &[u8] = b"history";
/// `Meta` row: the layout of the state tables. Absent: version 1 (`PlainAccountState`,
/// `PlainStorageState`, changesets keyed by address) or a datadir that holds nothing yet.
const SCHEMA_KEY: &[u8] = b"schema";
/// Version 2: state and changesets keyed by hash ([`migrate_v1`]).
const SCHEMA: u64 = 2;
const FSYNC_FAILED: &str = "FSYNC_FAILED";

pub struct Config {
    /// Blocks above the oldest openable one that keep a changeset (the unwind and state window).
    pub retention: u64,
    /// Blocks per segment chunk file.
    pub per_file: u64,
    /// `None`: ADR-005 as written, every block commit is on the device when it returns. `Some(t)`:
    /// group commit, for a node that does not sign. A block commit skips the journal, segment and
    /// directory syncs and MDBX runs `SafeNoSync`. [`MdbxStore::steady`] syncs the segment chunks the
    /// deferred blocks wrote to, then makes MDBX steady; it runs on the first block commit `t` after
    /// the last steady point, before every other write transaction, and when the owner calls it (the
    /// node does on close). A crash of the process loses nothing (the bytes are in the page cache); a
    /// crash of the system rolls MDBX back to its last steady commit, whose segment bytes were synced
    /// before it, and the restart cuts the frames above it (ADR-005 section 7) and fetches those
    /// blocks again. [`Store::durability`] is not `Durable` while blocks are deferred.
    pub group_commit: Option<std::time::Duration>,
    /// `--state.disk`: the head state is read from the `HashedAccounts` and `HashedStorages` tables
    /// behind bounded caches ([`DiskFlat`]) instead of a copy of every row in memory. The rows, the
    /// roots and the commit order are the same.
    pub disk_state: bool,
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
    #[error("this datadir has state-table schema {0}; this build reads up to {SCHEMA}. Use the build that wrote it")]
    Schema(u64),
}

/// Version 1 to 2, in one MDBX transaction (all or nothing): the address-keyed state tables become
/// the hash-keyed ones, the changesets are re-keyed, the plain tables are emptied. Every key is
/// hashed from the address the old row carries, so nothing is looked up and nothing can be wrong
/// but by a bug in `keccak256`. The state root is checked at load (`load_state`) as on any start.
fn migrate_v1(tx: &Wr) -> R<()> {
    for (k, v) in tx.range(T::PlainAccounts, &[], None)? {
        put_account(tx, &keccak256(&k), &v)?;
        put_preimage(tx, &k)?;
    }
    for (k, v) in tx.range(T::PlainStorage, &[], None)? {
        let (a, s) = k.split_at(k.len().min(20));
        tx.put(
            T::HashedStorages,
            &[keccak256(a).as_slice(), keccak256(s).as_slice()].concat(),
            &v,
        )?;
        put_preimage(tx, a)?;
        put_preimage(tx, s)?;
    }
    for (t, split) in [(T::AccountChangeSets, 28), (T::StorageChangeSets, 60)] {
        for (k, v) in tx.range(t, &[], None)? {
            let (n, rest) = k.split_at(8);
            let mut nk = n.to_vec();
            if split == 28 {
                nk.extend_from_slice(keccak256(rest).as_slice());
            } else {
                let (a, s) = rest.split_at(20);
                nk.extend_from_slice(keccak256(a).as_slice());
                nk.extend_from_slice(keccak256(s).as_slice());
            }
            tx.del(t, &k)?;
            tx.put(t, &nk, &v)?;
        }
    }
    tx.del_range(T::PlainAccounts, &[], None)?;
    tx.del_range(T::PlainStorage, &[], None)?;
    tx.put(T::Meta, SCHEMA_KEY, &SCHEMA.to_le_bytes())
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

/// Changeset keys: `block || keccak(address)` and `block || keccak(address) || keccak(slot)`.
fn cs_key(n: u64, ha: &B256) -> Vec<u8> {
    [&be(n)[..], ha.as_slice()].concat()
}

fn cs_skey(n: u64, ha: &B256, hs: &B256) -> Vec<u8> {
    [&be(n)[..], ha.as_slice(), hs.as_slice()].concat()
}

/// The `TxLookup` rows of block `n`'s transactions: `keccak(tx) -> n u64 BE || index u32 BE`.
fn put_tx_lookup(tx: &Wr, n: u64, body: &Body) -> R<()> {
    for (i, t) in body.transactions.iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        tx.put(
            T::TxLookup,
            keccak256(t).as_slice(),
            &[&be(n)[..], &i.to_be_bytes()].concat(),
        )?;
    }
    Ok(())
}

fn del_tx_lookup(tx: &Wr, body: &Body) -> R<()> {
    for t in &body.transactions {
        tx.del(T::TxLookup, keccak256(t).as_slice())?;
    }
    Ok(())
}

fn hashed_skey(ha: &B256, hs: &B256) -> Vec<u8> {
    [ha.as_slice(), hs.as_slice()].concat()
}

type R<V> = Result<V, MdbxError>;

fn put_account(tx: &Wr, ha: &B256, rlp: &[u8]) -> R<()> {
    tx.put(T::HashedAccounts, ha.as_slice(), rlp)
}

fn del_account(tx: &Wr, ha: &B256) -> R<()> {
    tx.del(T::HashedAccounts, ha.as_slice())
}

fn put_slot(tx: &Wr, ha: &B256, hs: &B256, v: &[u8]) -> R<()> {
    tx.put(T::HashedStorages, &hashed_skey(ha, hs), v)
}

fn del_slot(tx: &Wr, ha: &B256, hs: &B256) -> R<()> {
    tx.del(T::HashedStorages, &hashed_skey(ha, hs))
}

/// Keeps what execution knows: the address and the slot behind a hash. Never read back for
/// consensus; only to show an address (export, dump) and to serve xpre/1.
fn put_preimage(tx: &Wr, pre: &[u8]) -> R<()> {
    tx.put(T::Preimages, keccak256(pre).as_slice(), pre)
}

/// Writes the diff of block `n` into the state tables and its changeset (whole prior records) into
/// the changeset tables: the same semantics as `StateDiff::apply`, over the tables.
fn write_diff(tx: &Wr, n: u64, d: &StateDiff) -> R<()> {
    for (a, new) in &d.accounts {
        let ha = keccak256(a);
        let prior = match tx.get(T::HashedAccounts, ha.as_slice())? {
            None => vec![0],
            Some(r) => [&[1][..], &r].concat(),
        };
        tx.put(T::AccountChangeSets, &cs_key(n, &ha), &prior)?;
        if new.is_none() {
            for (k, v) in tx.prefix(T::HashedStorages, ha.as_slice())? {
                let hs = B256::from_slice(&k[32..]);
                tx.put(T::StorageChangeSets, &cs_skey(n, &ha, &hs), &v)?;
                del_slot(tx, &ha, &hs)?;
            }
        } else {
            put_preimage(tx, a.as_slice())?;
        }
    }
    for ((a, s), v) in &d.storage {
        if matches!(d.accounts.get(a), Some(None)) {
            continue;
        }
        let (ha, hs) = (keccak256(a), keccak256(s));
        let prior = tx
            .get(T::HashedStorages, &hashed_skey(&ha, &hs))?
            .unwrap_or_else(|| vec![0; 32]);
        tx.put(T::StorageChangeSets, &cs_skey(n, &ha, &hs), &prior)?;
        if v.is_zero() {
            del_slot(tx, &ha, &hs)?;
        } else {
            put_slot(tx, &ha, &hs, &v.to_be_bytes::<32>())?;
            put_preimage(tx, a.as_slice())?;
            put_preimage(tx, s.as_slice())?;
        }
    }
    for (a, new) in &d.accounts {
        let ha = keccak256(a);
        match new {
            Some(x) => put_account(tx, &ha, &x.rlp())?,
            None => del_account(tx, &ha)?,
        }
    }
    Ok(())
}

/// Writes the changeset rows of block `k` back into the state tables and deletes them.
fn revert_block(tx: &Wr, k: u64) -> R<()> {
    for (key, val) in tx.prefix(T::AccountChangeSets, &be(k))? {
        let ha = B256::from_slice(&key[8..40]);
        match val.split_first() {
            Some((1, rlp)) => put_account(tx, &ha, rlp)?,
            _ => del_account(tx, &ha)?,
        }
        tx.del(T::AccountChangeSets, &key)?;
    }
    for (key, val) in tx.prefix(T::StorageChangeSets, &be(k))? {
        let (ha, hs) = (
            B256::from_slice(&key[8..40]),
            B256::from_slice(&key[40..72]),
        );
        if val.iter().all(|b| *b == 0) {
            del_slot(tx, &ha, &hs)?;
        } else {
            put_slot(tx, &ha, &hs, &val)?;
        }
        tx.del(T::StorageChangeSets, &key)?;
    }
    Ok(())
}

fn changeset_of(rd: &Rd, k: u64) -> Result<ChangeSet, StoreError> {
    let bad = || StoreError::Damaged(format!("changeset of block {k}"));
    let mut cs = ChangeSet::default();
    for (key, val) in rd.prefix(T::AccountChangeSets, &be(k)).map_err(|_| bad())? {
        let a = B256::from_slice(key.get(8..40).ok_or_else(bad)?);
        let acc = match val.split_first() {
            Some((1, rlp)) => Some(dec_account(rlp).ok_or_else(bad)?),
            _ => None,
        };
        cs.accounts.insert(a, acc);
    }
    for (key, val) in rd.prefix(T::StorageChangeSets, &be(k)).map_err(|_| bad())? {
        let (a, s) = (
            B256::from_slice(key.get(8..40).ok_or_else(bad)?),
            B256::from_slice(key.get(40..72).ok_or_else(bad)?),
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
    /// The verified anchor manifest of a datadir bootstrapped from a snapshot.
    anchor: Option<Bytes>,
    /// Oldest openable block: rows of blocks at or below it have no changeset.
    base: u64,
    state: StateProvider<Flat>,
    side: HashMap<B256, Record>,
    stateless: HashMap<B256, Block>,
    degraded: Option<Cause>,
    reconciling: bool,
    dead: Option<Dead>,
    /// A fired `Probe::IoCommitted`: the commit in flight is applied before the error is returned.
    committed_on_error: bool,
    /// Group commit: the first block whose commit is not steady yet, and when MDBX was last made steady.
    deferred: Option<u64>,
    steady_at: std::time::Instant,
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
        let env = Env::open_with(path, &names, cfg.group_commit.is_none())?;
        let mut s = Self {
            env: Arc::new(env),
            segs: Segs::new(dir, cfg.per_file),
            state: StateProvider::new(Flat::Mem(MemFlat::default()), 0, B256::ZERO, cfg.retention),
            cfg,
            path: path.to_owned(),
            generation: 0,
            manifest: None,
            head: (0, B256::ZERO),
            committed: None,
            anchor: None,
            base: 0,
            side: HashMap::new(),
            stateless: HashMap::new(),
            degraded: None,
            reconciling: true,
            dead: None,
            committed_on_error: false,
            deferred: None,
            steady_at: std::time::Instant::now(),
        };
        let schema = {
            let rd = s.env.read()?;
            rd.get(T::Meta, SCHEMA_KEY)?.and_then(|b| le_u64(&b))
        };
        if let Some(v) = schema.filter(|v| *v > SCHEMA) {
            return Err(OpenError::Schema(v));
        }
        if schema.is_none() {
            let tx = s.env.write()?;
            migrate_v1(&tx)?;
            tx.commit()?;
        }
        let rd = s.env.read()?;
        s.generation = rd
            .get(T::Meta, GEN_KEY)?
            .and_then(|b| le_u64(&b))
            .unwrap_or(0);
        s.manifest = rd
            .get(T::Meta, MANIFEST_KEY)?
            .and_then(|b| Manifest::decode(&b).ok());
        s.committed = rd.get(T::Meta, COMMITTED_KEY)?.and_then(|b| dec_link(&b));
        s.anchor = rd.get(T::Meta, ANCHOR_KEY)?.map(Bytes::from);
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
        sync: bool,
    ) -> Result<(), StoreError> {
        let bytes = journal_bytes(kind, self.generation + 1, number, hash, payload);
        let mut f = self
            .segs
            .dir
            .create(JOURNAL_TMP)
            .map_err(|e| self.io_failed(&e))?;
        f.write_at(0, &bytes).map_err(|e| self.io_failed(&e))?;
        self.step(points.0)?;
        if sync {
            f.sync_all().map_err(|_| self.sync_failed())?;
        }
        if let Some(k) = points.1 {
            self.step(k)?;
        }
        self.segs
            .dir
            .rename(JOURNAL_TMP, JOURNAL)
            .map_err(|e| self.io_failed(&e))?;
        self.step(points.2)?;
        if !sync {
            return Ok(());
        }
        self.segs
            .dir
            .sync("journal")
            .map_err(|_| self.sync_failed())
    }

    /// S7 / U5. The head is already published: a failure here is fail-stop and the next start
    /// deletes the stale journal.
    fn remove_journal(
        &mut self,
        points: (KillPoint, KillPoint),
        sync: bool,
    ) -> Result<(), StoreError> {
        self.segs
            .dir
            .remove(JOURNAL)
            .map_err(|_| self.fail_stop(Cause::Io))?;
        self.step(points.0)?;
        if sync {
            self.segs
                .dir
                .sync("journal")
                .map_err(|_| self.sync_failed())?;
        }
        self.step(points.1)
    }

    // ---- group commit

    /// The commit of a write transaction other than a deferred block commit: the deferred blocks'
    /// segment bytes reach the device first, then the commit, which group commit makes steady.
    fn commit_steady(&mut self, tx: Wr<'_>) -> Result<(), StoreError> {
        self.sync_deferred()?;
        tx.commit().map_err(|_| self.sync_failed())?;
        self.sync_env()
    }

    /// Group commit: syncs the segment chunks and directories the deferred blocks wrote to.
    fn sync_deferred(&mut self) -> Result<(), StoreError> {
        let Some(from) = self.deferred else {
            return Ok(());
        };
        let per = self.cfg.per_file;
        let last = chunk_of(self.head.0.max(from), per);
        for s in Segment::ALL {
            let mut c = chunk_of(from, per);
            while c <= last {
                self.segs.sync_chunk(s, c).map_err(|_| self.sync_failed())?;
                c += per;
            }
        }
        self.segs.sync_dirs().map_err(|_| self.sync_failed())
    }

    fn sync_env(&mut self) -> Result<(), StoreError> {
        if self.cfg.group_commit.is_some() {
            self.env.sync().map_err(|_| self.sync_failed())?;
        }
        self.deferred = None;
        self.steady_at = std::time::Instant::now();
        Ok(())
    }

    /// Group commit: every committed block is on the device after this returns. A no-op otherwise.
    pub fn steady(&mut self) -> Result<(), StoreError> {
        if self.deferred.is_none() {
            return Ok(());
        }
        self.alive()?;
        self.sync_deferred()?;
        self.sync_env()
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

    /// The header of canonical block `n`, CRC-checked; `None` when `n` is not in the canonical index.
    pub fn canonical_header(&self, n: u64) -> Result<Option<Header>, StoreError> {
        let rd = self.rd()?;
        if rd
            .get(T::CanonicalHeaders, &be(n))
            .map_err(|e| StoreError::Damaged(e.to_string()))?
            .is_none()
        {
            return Ok(None);
        }
        self.header_at(&rd, n).map(Some)
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
    ///
    /// # Panics
    /// With `disk_state`: the rows are in the tables, not in a `MemFlat` (use [`Self::wide_accounts`]
    /// or [`Self::state_root`]).
    pub fn head_flat(&self) -> &MemFlat {
        match self.state.flat() {
            Flat::Mem(f) => f,
            Flat::Disk(_) => panic!("head_flat: the head state is on disk (--state.disk)"),
        }
    }

    /// `--state.disk` is on.
    pub fn disk_state(&self) -> bool {
        self.cfg.disk_state
    }

    /// Cache counters of the disk flat state; `None` for the in-memory one.
    pub fn disk_stats(&self) -> Option<diskflat::Stats> {
        match self.state.flat() {
            Flat::Disk(f) => Some(f.stats()),
            Flat::Mem(_) => None,
        }
    }

    /// The state root of the head, from the incremental trie (not a pass over every row).
    pub fn state_root(&self) -> B256 {
        self.state.root()
    }

    /// The accounts of the head state whose balance is above 2^256, by key.
    pub fn wide_accounts(&self) -> Vec<(B256, xone_state::Account)> {
        self.state
            .flat()
            .hashed_accounts()
            .filter(|(_, a)| a.balance.is_wide())
            .collect()
    }

    /// A read of the disk flat state failed while it was being loaded: the datadir cannot be used.
    fn flat_ok(&self, state: &StateProvider<Flat>) -> Result<(), StoreError> {
        match state.flat() {
            Flat::Disk(f) if f.failed() => Err(StoreError::Damaged(
                "state tables: a read failed while loading".into(),
            )),
            _ => Ok(()),
        }
    }

    /// A read of the disk flat state failed: stop before a block is committed on a wrong answer.
    fn flat_failed(&mut self) -> Result<(), StoreError> {
        if matches!(self.state.flat(), Flat::Disk(f) if f.failed()) {
            // stop; the tables are not at fault, so the next start may go on (no fsync guard)
            eprintln!("xone-db: a read of the disk state failed (fail-stop)");
            self.dead = Some(Dead::Failed(Cause::Io));
            return Err(StoreError::Degraded(Cause::Io));
        }
        Ok(())
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

    /// True when the datadir was made by a build that writes `TxLookup` with every block (`Meta`
    /// row `history`); an older one has no transaction index and no consensus rows.
    pub fn has_history_tables(&self) -> Result<bool, StoreError> {
        Ok(self
            .rd()?
            .get(T::Meta, HISTORY_KEY)
            .map_err(|e| StoreError::Damaged(e.to_string()))?
            .is_some())
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
        self.commit_steady(tx)?;
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
                tx.put(T::Meta, HISTORY_KEY, &[1])?;
                for (ha, acc) in flat.hashed_accounts() {
                    put_account(tx, &ha, &acc.rlp())?;
                    for (hs, v) in flat.hashed_slots(ha) {
                        put_slot(tx, &ha, &hs, &v.to_be_bytes::<32>())?;
                    }
                }
                for p in flat.preimages() {
                    put_preimage(tx, &p)?;
                }
            }
            None => {
                put_tx_lookup(tx, n, &b.body)?;
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
        // group commit defers the syncs of a block; the first commit of a datadir is always steady
        let defer = self.cfg.group_commit.is_some() && anchor.is_none();
        // S1 journal
        self.step(K01)?;
        self.write_journal(
            JOURNAL_COMMIT,
            n,
            b.hash(),
            &block_rlp(b),
            (K02, Some(K03), K04),
            !defer,
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
            if !defer {
                self.segs.sync_file(s, n).map_err(|_| self.sync_failed())?;
            }
            self.step([K10, K11, K12][i])?;
        }
        if !defer {
            self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
        }
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
        self.flat_failed()?;
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
        if defer {
            tx.commit().map_err(|_| self.sync_failed())?;
            self.deferred.get_or_insert(n);
        } else {
            self.commit_steady(tx)?;
        }
        self.degraded = None;
        self.publish_commit(rec, &manifest, committed, anchor.as_ref(), new_base)?;
        self.step(K16)?;
        // S7 publish: the visible lengths swap, then the journal goes
        self.segs.publish(&manifest);
        self.step(K17)?;
        self.remove_journal((K18, K19), !defer)?;
        match self.cfg.group_commit {
            Some(t) if defer && self.steady_at.elapsed() >= t => self.steady(),
            _ => Ok(()),
        }
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
                self.state = if self.cfg.disk_state {
                    // the tables hold the anchor rows: the state reads them
                    StateProvider::from_head(
                        Flat::Disk(DiskFlat::new(self.env.clone())),
                        (b.number(), b.hash()),
                        Vec::new(),
                        self.cfg.retention,
                    )
                } else {
                    StateProvider::new(
                        Flat::Mem(f.clone()),
                        b.number(),
                        b.hash(),
                        self.cfg.retention,
                    )
                };
            }
            None => {
                let r = if self.cfg.disk_state {
                    // the rows and the changeset are in the tables: the model takes what they hold
                    let cs = self.rd().and_then(|rd| changeset_of(&rd, b.number()))?;
                    self.state
                        .commit_written(b.hash(), b.parent(), &rec.diff, cs)
                } else {
                    self.state.commit_block(b.hash(), b.parent(), &rec.diff)
                };
                if let Err(e) = r {
                    // The tables hold the block and the model could not take it: stop, a restart reloads.
                    self.fail_stop(Cause::Io);
                    return Err(e.into());
                }
                self.flat_failed()?;
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
        let t0 = std::time::Instant::now();
        let (head_n, head_h) = (m.head_number, m.head_hash);
        let bad = |what: &str| StoreError::Damaged(format!("state tables: {what}"));
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.rd()?;
        let head_header = self.header_at(&rd, head_n)?;
        if head_header.hash() != head_h {
            return Err(bad("the head header is not the head of the manifest"));
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
        let base_hash = *hashes
            .get(&base)
            .ok_or_else(|| bad("no canonical hash at the base"))?;
        let state = if self.cfg.disk_state {
            // The tables hold the head; the retained changesets are rows. Nothing is loaded or
            // replayed: the trie is built from one paged scan and its root is the check.
            let mut chain = Vec::new();
            for k in base + 1..=head_n {
                let hash = *hashes
                    .get(&k)
                    .ok_or_else(|| bad("a hole in the canonical index"))?;
                chain.push((k, hash, changeset_of(&rd, k)?));
            }
            let flat = DiskFlat::new(self.env.clone());
            let state = StateProvider::from_head(
                Flat::Disk(flat),
                (base, base_hash),
                chain,
                self.cfg.retention,
            );
            self.flat_ok(&state)?;
            if state.root() != head_header.state_root {
                return Err(bad("the state tables do not give the head's state root"));
            }
            state
        } else {
            let mut flat = MemFlat::default();
            for (k, v) in rd.range(T::HashedAccounts, &[], None).map_err(db)? {
                flat.set_hashed_account(
                    B256::from_slice(k.get(..32).ok_or_else(|| bad("account key"))?),
                    Some(dec_account(&v).ok_or_else(|| bad("account row"))?),
                );
            }
            for (k, v) in rd.range(T::HashedStorages, &[], None).map_err(db)? {
                flat.set_hashed_slot(
                    B256::from_slice(k.get(..32).ok_or_else(|| bad("slot key"))?),
                    B256::from_slice(k.get(32..64).ok_or_else(|| bad("slot key"))?),
                    U256::from_be_slice(&v),
                );
            }
            if flat_root(&flat) != head_header.state_root {
                return Err(bad("the flat state does not give the head's state root"));
            }
            for k in (base + 1..=head_n).rev() {
                changeset_of(&rd, k)?.revert(&mut flat);
            }
            let mut state =
                StateProvider::new(Flat::Mem(flat), base, base_hash, self.cfg.retention);
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
            state
        };
        eprintln!(
            "state: head {head_n} loaded from the tables in {} ms ({}, {} blocks of changesets)",
            t0.elapsed().as_millis(),
            if self.cfg.disk_state {
                "disk"
            } else {
                "memory"
            },
            head_n - base
        );
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
        self.commit_steady(tx)?;
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

/// Reads of committed rows that do not go through the store's owner, so they never wait for an import
/// and an import never waits for them: each call is one MDBX read transaction, and a header is read
/// from its segment frame (CRC-checked) and checked against the canonical index. The node's history
/// reads what is below its memory window through it.
pub struct StoreReader<D: SegDir> {
    env: Arc<Env>,
    segs: Segs<D>,
}

impl<D: SegDir + Clone> MdbxStore<D> {
    pub fn reader(&self) -> StoreReader<D> {
        StoreReader {
            env: self.env.clone(),
            segs: Segs::new(self.segs.dir.clone(), self.cfg.per_file),
        }
    }
}

impl<D: SegDir> StoreReader<D> {
    fn get(&self, t: T, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        self.env.read().map_err(db)?.get(t, key).map_err(db)
    }

    /// The canonical hash of block `n`.
    pub fn hash(&self, n: u64) -> Result<Option<B256>, StoreError> {
        match self.get(T::CanonicalHeaders, &be(n))? {
            Some(b) if b.len() == 32 => Ok(Some(B256::from_slice(&b))),
            Some(_) => Err(bad_row("canonical index", n)),
            None => Ok(None),
        }
    }

    /// The number of canonical block `hash`.
    pub fn number(&self, hash: B256) -> Result<Option<u64>, StoreError> {
        self.get(T::HeaderNumbers, hash.as_slice())?
            .map(|b| be_u64(&b).ok_or_else(|| StoreError::Damaged(format!("number of {hash}"))))
            .transpose()
    }

    /// The header of canonical block `n`.
    pub fn header(&self, n: u64) -> Result<Option<Header>, StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.env.read().map_err(db)?;
        let Some(hash) = rd.get(T::CanonicalHeaders, &be(n)).map_err(db)? else {
            return Ok(None);
        };
        if hash.len() != 32 {
            return Err(bad_row("canonical index", n));
        }
        let start = rd
            .get(T::SegIdx, &seg_key(Segment::Headers, n))
            .map_err(db)?
            .and_then(|b| le_u64(&b))
            .ok_or_else(|| bad_row("header index", n))?;
        let h = self
            .segs
            .read(Segment::Headers, n, start)
            .map_err(|e| StoreError::Damaged(e.to_string()))
            .and_then(|b| dec_header(&b).ok_or_else(|| bad_row("header", n)))?;
        if h.hash() != B256::from_slice(&hash) {
            return Err(bad_row("header (hash differs from the index)", n));
        }
        Ok(Some(h))
    }

    /// The frame of segment `s` of canonical block `n`, which must be `hash`: `Ok(None)` for a block
    /// that is not in the canonical index, has another hash, or whose rows the store does not hold
    /// (below the retention floor of a minimal node the index rows are deleted). One MDBX read
    /// transaction and the segment file, no store lock and no import lock, so a request for old blocks
    /// never waits for an import (geth-XDC `GetBodyRLP` reads the database with no chain lock,
    /// `core/blockchain_reader.go:130-146`).
    fn frame_of(&self, s: Segment, n: u64, hash: B256) -> Result<Option<Vec<u8>>, StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.env.read().map_err(db)?;
        if rd.get(T::CanonicalHeaders, &be(n)).map_err(db)?.as_deref() != Some(hash.as_slice()) {
            return Ok(None);
        }
        let Some(start) = rd
            .get(T::SegIdx, &seg_key(s, n))
            .map_err(db)?
            .and_then(|b| le_u64(&b))
        else {
            return Ok(None);
        };
        self.segs
            .read(s, n, start)
            .map(Some)
            .map_err(|e| StoreError::Damaged(e.to_string()))
    }

    /// The body of canonical block `hash` (number `n`), see [`Self::frame_of`].
    pub fn body(&self, n: u64, hash: B256) -> Result<Option<Body>, StoreError> {
        self.frame_of(Segment::Bodies, n, hash)?
            .map(|f| dec_body(&f).ok_or_else(|| bad_row("body", n)))
            .transpose()
    }

    /// The receipts of canonical block `hash` (number `n`), see [`Self::frame_of`].
    pub fn receipts(&self, n: u64, hash: B256) -> Result<Option<Vec<Bytes>>, StoreError> {
        self.frame_of(Segment::Receipts, n, hash)?
            .map(|f| dec_receipts(&f).ok_or_else(|| bad_row("receipts", n)))
            .transpose()
    }

    /// Block number and index of transaction `hash` in the canonical chain.
    pub fn tx(&self, hash: B256) -> Result<Option<(u64, u32)>, StoreError> {
        self.get(T::TxLookup, hash.as_slice())?
            .map(|b| {
                let n = be_u64(&b);
                let i = b
                    .get(8..12)
                    .and_then(|i| i.try_into().ok())
                    .map(u32::from_be_bytes);
                n.zip(i)
                    .ok_or_else(|| StoreError::Damaged(format!("transaction index row of {hash}")))
            })
            .transpose()
    }

    /// The row of block `n` in a block-keyed table.
    pub fn row(&self, t: T, n: u64) -> Result<Option<Vec<u8>>, StoreError> {
        self.get(t, &be(n))
    }

    /// Every row of a block-keyed table, ascending.
    pub fn rows(&self, t: T) -> Result<Vec<(u64, Vec<u8>)>, StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        self.env
            .read()
            .map_err(db)?
            .range(t, &[], None)
            .map_err(db)?
            .into_iter()
            .map(|(k, v)| Ok((be_u64(&k).ok_or_else(|| bad_row(t.name(), 0))?, v)))
            .collect()
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
        self.write_journal(JOURNAL_UNWIND, number, target, &[], (U1a, None, U1b), true)?;
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
                del_tx_lookup(&tx, &rec.block.body)?;
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
        self.commit_steady(tx)?;
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
        self.remove_journal((U5a, U5b), true)
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
            (None, None, false) if self.deferred.is_some() => {
                Durability::Degraded(Cause::GroupCommit)
            }
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

    fn state_rows(&self, hash: B256) -> Result<StateRows, Missing> {
        if self.cfg.disk_state {
            // the whole state would be copied to memory: a node on disk state serves no snapshot
            return Err(Missing::Block(hash));
        }
        let (accounts, storage) = self.state.rows_at(hash)?;
        Ok(StateRows { accounts, storage })
    }

    fn preimage(&self, hash: B256) -> Option<Bytes> {
        let rd = self.env.read().ok()?;
        rd.get(T::Preimages, hash.as_slice()).ok()?.map(Bytes::from)
    }

    fn anchor(&self) -> Option<Bytes> {
        self.anchor.clone()
    }

    fn install_snapshot(&mut self, mut s: Install) -> Result<(), StoreError> {
        self.alive()?;
        if self.manifest.is_some() || self.generation != 0 || self.has_records() {
            return Err(StoreError::NotEmpty);
        }
        let bad = StoreError::BadInstall;
        let (Some(pivot), Some(top)) = (s.records.last(), s.headers.last()) else {
            return Err(bad("no blocks"));
        };
        if pivot.block.hash() != top.hash() || s.headers.len() < s.records.len() {
            return Err(bad("the last header is not the last record"));
        }
        if !s
            .headers
            .windows(2)
            .all(|w| w[1].number == w[0].number + 1 && w[1].parent_hash == w[0].hash())
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
            if !self.cfg.disk_state {
                flat.set_hashed_account(*a, Some(x.clone()));
            }
        }
        if !self.cfg.disk_state {
            for (a, k, v) in &s.state.storage {
                flat.set_hashed_slot(*a, *k, *v);
            }
        }
        // the root is streamed over the sorted rows: hashing `flat` would copy the whole state again
        s.state.accounts.sort_unstable_by_key(|(h, _)| *h);
        s.state.storage.sort_unstable_by_key(|(a, k, _)| (*a, *k));
        if s.state.accounts.windows(2).any(|w| w[0].0 == w[1].0)
            || s.state
                .storage
                .windows(2)
                .any(|w| (w[0].0, w[0].1) == (w[1].0, w[1].1))
        {
            return Err(bad("duplicate state rows"));
        }
        if root_of_sorted_rows(&s.state.accounts, &s.state.storage) != pivot.block.header.state_root
        {
            return Err(bad("the state does not give the pivot's state root"));
        }
        let (h_first, r_first) = (s.headers[0].number, s.records[0].block.number());
        let per = self.cfg.per_file;
        // Frames per segment and chunk; `starts[i][n]` is the frame offset inside its chunk.
        let mut chunks: [BTreeMap<u64, Vec<u8>>; 3] = Default::default();
        let mut starts: [BTreeMap<u64, u64>; 3] = Default::default();
        let mut put = |i: usize, n: u64, payload: &[u8]| {
            let buf = chunks[i].entry(chunk_of(n, per)).or_default();
            starts[i].insert(n, HEADER_LEN + u64::try_from(buf.len()).unwrap_or(u64::MAX));
            buf.extend_from_slice(&frame(n, payload));
        };
        for h in &s.headers {
            put(0, h.number, &header_rlp(h));
        }
        for r in &s.records {
            put(1, r.block.number(), &body_rlp(&r.block.body));
            put(2, r.block.number(), &receipts_rlp(&r.receipts));
        }
        let mut entries = [SegmentEntry {
            segment: Segment::Headers,
            first_block: 0,
            last_block: number,
            len: 0,
            tail_digest: B256::ZERO,
        }; 3];
        for (i, sg) in Segment::ALL.into_iter().enumerate() {
            for (c, buf) in &chunks[i] {
                self.segs
                    .write_chunk(sg, *c, buf)
                    .map_err(|e| self.seg_failed(&e))?;
            }
            let (_, last) = chunks[i].iter().next_back().ok_or(bad("no frames"))?;
            let at = starts[i][&number];
            entries[i] = SegmentEntry {
                segment: sg,
                first_block: if i == 0 { h_first } else { r_first },
                last_block: number,
                len: HEADER_LEN + u64::try_from(last.len()).unwrap_or(u64::MAX),
                tail_digest: keccak256(&last[usize::try_from(at - HEADER_LEN).unwrap_or(0)..]),
            };
        }
        self.segs.sync_dirs().map_err(|_| self.sync_failed())?;
        let manifest = Manifest {
            generation: 1,
            head_number: number,
            head_hash: hash,
            entries,
        };
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        let r = (|| -> R<()> {
            for (a, x) in &s.state.accounts {
                put_account(&tx, a, &x.rlp())?;
            }
            for (a, k, v) in &s.state.storage {
                put_slot(&tx, a, k, &v.to_be_bytes::<32>())?;
            }
            for (h, p) in &s.preimages {
                // kept only when it proves itself, with the length of an address or of a slot
                if keccak256(p) == *h && matches!(p.len(), 20 | 32) {
                    tx.put(T::Preimages, h.as_slice(), p)?;
                }
            }
            for h in &s.headers {
                tx.put(T::CanonicalHeaders, &be(h.number), h.hash().as_slice())?;
                tx.put(T::HeaderNumbers, h.hash().as_slice(), &be(h.number))?;
                tx.put(
                    T::SegIdx,
                    &seg_key(Segment::Headers, h.number),
                    &starts[0][&h.number].to_le_bytes(),
                )?;
            }
            for r in &s.records {
                let n = r.block.number();
                for (i, sg) in [(1, Segment::Bodies), (2, Segment::Receipts)] {
                    tx.put(T::SegIdx, &seg_key(sg, n), &starts[i][&n].to_le_bytes())?;
                }
                put_tx_lookup(&tx, n, &r.block.body)?;
            }
            tx.put(T::Meta, HISTORY_KEY, &[1])?;
            if !s.anchor.is_empty() {
                tx.put(T::Meta, ANCHOR_KEY, &s.anchor)?;
            }
            Self::meta_rows(&tx, &manifest)
        })();
        r.map_err(|e| self.db_failed(&e))?;
        self.commit_steady(tx)?;
        self.generation = 1;
        self.manifest = Some(manifest.clone());
        self.head = (number, hash);
        self.base = number;
        self.state = if self.cfg.disk_state {
            StateProvider::from_head(
                Flat::Disk(DiskFlat::new(self.env.clone())),
                (number, hash),
                Vec::new(),
                s.retention,
            )
        } else {
            StateProvider::new(Flat::Mem(flat), number, hash, s.retention)
        };
        self.anchor = (!s.anchor.is_empty()).then_some(s.anchor);
        self.segs.publish(&manifest);
        self.reconciling = false;
        Ok(())
    }

    fn prune(&mut self, headers_from: u64, records_from: u64) -> Result<(), StoreError> {
        use KillPoint::*;
        self.alive()?;
        let (head, _) = self.head;
        let Some(old) = self.manifest.clone() else {
            return Err(StoreError::Unwind(0));
        };
        if headers_from > records_from || records_from > head {
            return Err(StoreError::Prune {
                headers_from,
                records_from,
                head,
            });
        }
        let h_from = headers_from.max(old.entries[0].first_block);
        let r_from = records_from.max(old.entries[1].first_block);
        self.step(PR1)?;
        let mut m = old;
        m.generation += 1;
        for (i, e) in m.entries.iter_mut().enumerate() {
            e.first_block = if i == 0 { h_from } else { r_from };
        }
        let env = self.env.clone();
        let tx = env.write().map_err(|e| self.db_failed(&e))?;
        let r = (|| -> R<()> {
            let upto = be(h_from);
            for (k, v) in tx.range(T::CanonicalHeaders, &[], Some(&upto))? {
                tx.del(T::HeaderNumbers, &v)?;
                tx.del(T::CanonicalHeaders, &k)?;
            }
            for s in Segment::ALL {
                let f = if s == Segment::Headers {
                    h_from
                } else {
                    r_from
                };
                tx.del_range(T::SegIdx, &seg_key(s, 0), Some(&seg_key(s, f)))?;
            }
            let upto = be(r_from);
            for t in [
                T::BlockDiffs,
                T::Aux,
                T::AccountChangeSets,
                T::StorageChangeSets,
            ] {
                tx.del_range(t, &[], Some(&upto))?;
            }
            let upto = be(h_from);
            for t in [T::ConsensusEpoch, T::ConsensusSnapshot] {
                tx.del_range(t, &[], Some(&upto))?;
            }
            Self::meta_rows(&tx, &m)
        })();
        r.map_err(|e| self.db_failed(&e))?;
        self.commit_steady(tx)?;
        self.generation = m.generation;
        self.manifest = Some(m.clone());
        self.step(PR2)?;
        self.unlink_below(&m, true)
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
