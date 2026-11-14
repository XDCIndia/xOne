//! The MDBX environment of ADR-005 (section 4) and the safety store of ADR-010 on its own
//! environment. `libmdbx` is the only crate in the tree that holds `unsafe` for this (the audit
//! row is in `docs/assurance/`); nothing here does.
//!
//! Open mode: `SyncMode::Durable`, WRITEMAP off. A commit that returned is on the device, so the
//! commit is the publication point of a generation. Tables are created once, at open; a table
//! name is part of the on-disk contract. A follower that does not sign may open it in group-commit
//! mode instead ([`Env::open_with`]): its commits become steady at [`Env::sync`].
use std::{borrow::Cow, fs, path::Path};

use libmdbx::{
    Database, DatabaseOptions, Mode, NoWriteMap, ReadWriteOptions, SyncMode, Table, TableFlags,
    Transaction, TransactionKind, WriteFlags, RO, RW,
};

use crate::{
    safety::{Ack, Loaded, SafetyStore, StoreError},
    Cause,
};

/// The tables of `chain.mdbx`. Block-keyed tables put the block number first (big endian), so an
/// unwind deletes a key range; the others carry a changeset (ADR-005 section 1, unwind rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum T {
    /// `gen` u64 LE, `manifest` 257 bytes, `committed` (the committed V2 block).
    Meta,
    /// u64 BE to hash.
    CanonicalHeaders,
    /// hash to u64 BE.
    HeaderNumbers,
    /// `segment u8 || block u64 BE` to frame start offset u64 LE: headers, bodies and receipts.
    SegIdx,
    /// Schema 1 only (address to account RLP): read and emptied by the migration, never written.
    PlainAccounts,
    /// Schema 1 only (`address || slot` to 32 bytes): see [`T::PlainAccounts`].
    PlainStorage,
    /// code hash to code.
    Bytecodes,
    /// keccak(address) to account RLP: the canonical account state (what snap serves and heals from).
    HashedAccounts,
    /// `keccak(address) || keccak(slot)` to 32 bytes: the canonical storage state.
    HashedStorages,
    /// hash to the address (20 bytes) or slot (32 bytes) it came from. Optional: filled by execution
    /// and installs, used only to show an address; no read of state needs it.
    Preimages,
    /// `block || keccak(address)` to `0` (absent) or `1 || account RLP`: the whole prior record.
    AccountChangeSets,
    /// `block || keccak(address) || keccak(slot)` to the prior 32 bytes.
    StorageChangeSets,
    /// block to the new values the block wrote (kept for a reorg that re-adopts it).
    BlockDiffs,
    /// block to the executor's own rows (`Record::aux`).
    Aux,
    /// hash to an executed block that is not canonical.
    SideBlocks,
    /// hash to a block stored without parent state.
    Stateless,
    ConsensusEpoch,
    ConsensusSnapshot,
    /// stage name to the checkpoint, u64 BE.
    Stages,
    StagedHeaders,
    StagedBodies,
    /// transaction hash to `block u64 BE || index u32 BE`, for every canonical block's transactions.
    TxLookup,
    /// `--state.disk`: the nodes of the state tries of 32 bytes or more, by position: `0 ||
    /// nibbles` for the account trie, `1 || keccak(address) || nibbles` for a storage trie (one byte
    /// per nibble), to the node's RLP. The `Meta` row `trieroot` says which state root they are.
    TrieNodes,
    /// A snap download in progress: `p` to the pivot root and the cursors, `1 || keccak(address)` to
    /// the storage cursor of that account (see `xone_sync::mdbx::snapstate`). Emptied by the install.
    SnapProgress,
    /// `--gcmode archive` only: `keccak(address) || block u64 BE` to the account before `block` changed
    /// it (the [`T::AccountChangeSets`] value). The state of account `a` after block `n` is the first
    /// row of `a` above `n`, else `HashedAccounts`: one cursor seek. An unwind deletes the rows of the
    /// blocks it removes.
    AccountHistory,
    /// `--gcmode archive` only: `keccak(address) || keccak(slot) || block u64 BE` to the prior 32
    /// bytes, read as [`T::AccountHistory`] over `HashedStorages`.
    StorageHistory,
}

impl T {
    pub const ALL: [T; 26] = [
        T::Meta,
        T::CanonicalHeaders,
        T::HeaderNumbers,
        T::SegIdx,
        T::PlainAccounts,
        T::PlainStorage,
        T::Bytecodes,
        T::HashedAccounts,
        T::HashedStorages,
        T::Preimages,
        T::AccountChangeSets,
        T::StorageChangeSets,
        T::BlockDiffs,
        T::Aux,
        T::SideBlocks,
        T::Stateless,
        T::ConsensusEpoch,
        T::ConsensusSnapshot,
        T::Stages,
        T::StagedHeaders,
        T::StagedBodies,
        T::TxLookup,
        T::TrieNodes,
        T::SnapProgress,
        T::AccountHistory,
        T::StorageHistory,
    ];

    pub fn name(self) -> &'static str {
        match self {
            T::Meta => "Meta",
            T::CanonicalHeaders => "CanonicalHeaders",
            T::HeaderNumbers => "HeaderNumbers",
            T::SegIdx => "SegIdx",
            T::PlainAccounts => "PlainAccountState",
            T::PlainStorage => "PlainStorageState",
            T::Bytecodes => "Bytecodes",
            T::HashedAccounts => "HashedAccounts",
            T::HashedStorages => "HashedStorages",
            T::Preimages => "Preimages",
            T::AccountChangeSets => "AccountChangeSets",
            T::StorageChangeSets => "StorageChangeSets",
            T::BlockDiffs => "BlockDiffs",
            T::Aux => "Aux",
            T::SideBlocks => "SideBlocks",
            T::Stateless => "Stateless",
            T::ConsensusEpoch => "ConsensusEpoch",
            T::ConsensusSnapshot => "ConsensusSnapshot",
            T::Stages => "Stages",
            T::StagedHeaders => "StagedHeaders",
            T::StagedBodies => "StagedBodies",
            T::TxLookup => "TxLookup",
            T::TrieNodes => "TrieNodes",
            T::SnapProgress => "SnapProgress",
            T::AccountHistory => "AccountHistory",
            T::StorageHistory => "StorageHistory",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("mdbx: {0}")]
pub struct MdbxError(#[from] pub libmdbx::Error);

impl MdbxError {
    /// A full map or disk is `NoSpace`; every other failure is `Io`.
    pub fn cause(&self) -> Cause {
        use libmdbx::Error as E;
        match &self.0 {
            E::MapFull | E::UnableExtendMapsize | E::Other(28) => Cause::NoSpace,
            _ => Cause::Io,
        }
    }
}

type R<V> = Result<V, MdbxError>;

fn options(sync_mode: SyncMode) -> DatabaseOptions {
    DatabaseOptions {
        max_tables: Some(64),
        mode: Mode::ReadWrite(ReadWriteOptions {
            sync_mode,
            max_size: Some(1 << 40),
            growth_step: Some(64 << 20),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// An open `chain.mdbx`.
pub struct Env {
    db: Database<NoWriteMap>,
}

/// A transaction. [`Rd`] reads, [`Wr`] also writes; dropping a `Wr` without `commit` aborts it.
pub struct Tx<'e, K: TransactionKind> {
    tx: Transaction<'e, K, NoWriteMap>,
}
pub type Rd<'e> = Tx<'e, RO>;
pub type Wr<'e> = Tx<'e, RW>;

impl Env {
    pub fn open(path: &Path, tables: &[&str]) -> R<Env> {
        Self::open_with(path, tables, true)
    }

    /// [`Env::open`]; with `durable` false the environment is `SyncMode::SafeNoSync`: a commit is not
    /// flushed, a crash of the system rolls the database back to the last steady commit (never to a
    /// torn one), and [`Env::sync`] makes the last commit steady. For group commit only.
    pub fn open_with(path: &Path, tables: &[&str], durable: bool) -> R<Env> {
        fs::create_dir_all(path).map_err(|_| libmdbx::Error::Access)?;
        let mode = if durable {
            SyncMode::Durable
        } else {
            SyncMode::SafeNoSync
        };
        let db = Database::<NoWriteMap>::open_with_options(path, options(mode))?;
        let env = Env { db };
        let tx = env.db.begin_rw_txn()?;
        for t in tables {
            tx.create_table(Some(t), TableFlags::default())?;
        }
        tx.commit()?;
        Ok(env)
    }

    /// Flushes the environment: the last commit is on the device and steady.
    pub fn sync(&self) -> R<()> {
        self.db.sync(true)?;
        Ok(())
    }

    pub fn read(&self) -> R<Rd<'_>> {
        Ok(Tx {
            tx: self.db.begin_ro_txn()?,
        })
    }

    /// The one write transaction. A second caller blocks until the first commits or aborts.
    pub fn write(&self) -> R<Wr<'_>> {
        Ok(Tx {
            tx: self.db.begin_rw_txn()?,
        })
    }
}

/// The smallest key above every key that starts with `prefix`; `None` when there is none.
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut e = prefix.to_vec();
    while let Some(last) = e.pop() {
        if last != 0xff {
            e.push(last + 1);
            return Some(e);
        }
    }
    None
}

impl<K: TransactionKind> Tx<'_, K> {
    fn table(&self, name: &str) -> R<Table<'_>> {
        Ok(self.tx.open_table(Some(name))?)
    }

    pub fn get(&self, t: T, key: &[u8]) -> R<Option<Vec<u8>>> {
        self.get_in(t.name(), key)
    }

    pub fn get_in(&self, table: &str, key: &[u8]) -> R<Option<Vec<u8>>> {
        let tb = self.table(table)?;
        Ok(self.tx.get::<Cow<'_, [u8]>>(&tb, key)?.map(Cow::into_owned))
    }

    /// Rows with `from <= key < to` (no upper bound for `None`), ascending.
    pub fn range(&self, t: T, from: &[u8], to: Option<&[u8]>) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
        self.range_limit(t, from, to, usize::MAX)
    }

    /// [`Self::range`], at most `limit` rows: one page of a scan that must not hold a transaction
    /// (and a copy of the table) for its whole length.
    pub fn range_limit(
        &self,
        t: T,
        from: &[u8],
        to: Option<&[u8]>,
        limit: usize,
    ) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
        let tb = self.table(t.name())?;
        let mut cur = self.tx.cursor(&tb)?;
        let mut out = Vec::new();
        for kv in cur.iter_from::<Cow<'_, [u8]>, Cow<'_, [u8]>>(from) {
            let (k, v) = kv?;
            if &*k < from {
                // `iter_from` past the last key answers the last row: it is not in the range
                continue;
            }
            if out.len() >= limit || to.is_some_and(|to| &*k >= to) {
                break;
            }
            out.push((k.into_owned(), v.into_owned()));
        }
        Ok(out)
    }

    /// Rows whose key starts with `prefix`. The scan stops at the end of the prefix: it does not read
    /// (and copy) the rest of the table.
    pub fn prefix(&self, t: T, prefix: &[u8]) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
        let to = prefix_end(prefix);
        Ok(self
            .range(t, prefix, to.as_deref())?
            .into_iter()
            .take_while(|(k, _)| k.starts_with(prefix))
            .collect())
    }

    pub fn first(&self, t: T) -> R<Option<(Vec<u8>, Vec<u8>)>> {
        let tb = self.table(t.name())?;
        let mut cur = self.tx.cursor(&tb)?;
        Ok(cur
            .first::<Cow<'_, [u8]>, Cow<'_, [u8]>>()?
            .map(|(k, v)| (k.into_owned(), v.into_owned())))
    }

    pub fn last(&self, t: T) -> R<Option<(Vec<u8>, Vec<u8>)>> {
        let tb = self.table(t.name())?;
        let mut cur = self.tx.cursor(&tb)?;
        Ok(cur
            .last::<Cow<'_, [u8]>, Cow<'_, [u8]>>()?
            .map(|(k, v)| (k.into_owned(), v.into_owned())))
    }

    /// The number of rows of a table (from its metadata, no row is visited).
    pub fn entries(&self, t: T) -> R<usize> {
        let tb = self.table(t.name())?;
        Ok(self.tx.table_stat(&tb)?.entries())
    }

    pub fn is_empty(&self, t: T) -> R<bool> {
        let tb = self.table(t.name())?;
        let mut cur = self.tx.cursor(&tb)?;
        Ok(cur.first::<Cow<'_, [u8]>, Cow<'_, [u8]>>()?.is_none())
    }
}

impl Tx<'_, RW> {
    pub fn put(&self, t: T, key: &[u8], val: &[u8]) -> R<()> {
        self.put_in(t.name(), key, val)
    }

    pub fn put_in(&self, table: &str, key: &[u8], val: &[u8]) -> R<()> {
        let tb = self.table(table)?;
        Ok(self.tx.put(&tb, key, val, WriteFlags::UPSERT)?)
    }

    pub fn del(&self, t: T, key: &[u8]) -> R<()> {
        let tb = self.table(t.name())?;
        self.tx.del(&tb, key, None)?;
        Ok(())
    }

    /// Empties a table (fast: its pages are freed, no row is visited).
    pub fn clear(&self, t: T) -> R<()> {
        let tb = self.table(t.name())?;
        self.tx.clear_table(&tb)?;
        Ok(())
    }

    /// Deletes rows with `from <= key < to`.
    pub fn del_range(&self, t: T, from: &[u8], to: Option<&[u8]>) -> R<()> {
        for (k, _) in self.range(t, from, to)? {
            self.del(t, &k)?;
        }
        Ok(())
    }

    /// The commit. When it returns `Ok` the transaction is on the device (`Durable`).
    pub fn commit(self) -> R<()> {
        self.tx.commit()?;
        Ok(())
    }
}

// ---- the safety store (ADR-010): its own environment, so that nothing that unwinds or restores
// chain data can touch it.

const SAFETY_TABLE: &str = "Safety";
const SAFETY_KEY: &[u8] = b"pair";

/// `SafetyStore` over `safety.mdbx`: one row, replaced in one transaction. `persist_intent`
/// returns after a durable commit.
pub struct MdbxSafety {
    env: Env,
    seq: u64,
}

fn encode_pair(seq: u64, intent: &[u8], record: &[u8]) -> Vec<u8> {
    let mut v = seq.to_le_bytes().to_vec();
    v.extend_from_slice(
        &u32::try_from(intent.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    v.extend_from_slice(intent);
    v.extend_from_slice(record);
    v
}

fn decode_pair(b: &[u8]) -> Option<(u64, Loaded)> {
    let seq = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
    let n = usize::try_from(u32::from_le_bytes(b.get(8..12)?.try_into().ok()?)).ok()?;
    let intent = b.get(12..12 + n)?.to_vec();
    Some((
        seq,
        Loaded {
            intent,
            record: b.get(12 + n..)?.to_vec(),
        },
    ))
}

impl MdbxSafety {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let env = Env::open(path, &[SAFETY_TABLE]).map_err(|_| StoreError::Io)?;
        let seq = match env.read().and_then(|r| r.get_in(SAFETY_TABLE, SAFETY_KEY)) {
            Ok(Some(b)) => decode_pair(&b).ok_or(StoreError::Corrupt)?.0,
            Ok(None) => 0,
            Err(_) => return Err(StoreError::Io),
        };
        Ok(Self { env, seq })
    }
}

impl SafetyStore for MdbxSafety {
    fn load(&mut self) -> Result<Option<Loaded>, StoreError> {
        let row = self
            .env
            .read()
            .and_then(|r| r.get_in(SAFETY_TABLE, SAFETY_KEY))
            .map_err(|_| StoreError::Io)?;
        match row {
            None => Ok(None),
            Some(b) => Ok(Some(decode_pair(&b).ok_or(StoreError::Corrupt)?.1)),
        }
    }

    fn persist_intent(&mut self, intent: &[u8], record: &[u8]) -> Result<Ack, StoreError> {
        let seq = self.seq + 1;
        let tx = self.env.write().map_err(|_| StoreError::Io)?;
        tx.put_in(SAFETY_TABLE, SAFETY_KEY, &encode_pair(seq, intent, record))
            .map_err(|_| StoreError::Io)?;
        tx.commit().map_err(|_| StoreError::Io)?;
        self.seq = seq;
        Ok(Ack { seq })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("xone-db-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn rows_ranges_and_abort() {
        let d = tmp("env");
        let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
        let env = Env::open(&d, &names).unwrap();
        {
            let w = env.write().unwrap();
            for i in 0u8..5 {
                w.put(T::SegIdx, &[1, i], &[i]).unwrap();
            }
            w.commit().unwrap();
        }
        {
            let w = env.write().unwrap();
            w.del_range(T::SegIdx, &[1, 2], None).unwrap();
            // dropped: aborted
        }
        let r = env.read().unwrap();
        assert_eq!(r.range(T::SegIdx, &[1, 1], Some(&[1, 4])).unwrap().len(), 3);
        assert_eq!(r.prefix(T::SegIdx, &[1]).unwrap().len(), 5);
        assert_eq!(r.prefix(T::SegIdx, &[1, 3]).unwrap().len(), 1);
        assert!(r.prefix(T::SegIdx, &[2]).unwrap().is_empty());
        assert_eq!(prefix_end(&[1, 0xff]), Some(vec![2]));
        assert_eq!(prefix_end(&[0xff]), None);
        // a start above the last key holds no row (it must not answer the last one)
        assert!(r.range(T::SegIdx, &[1, 9], None).unwrap().is_empty());
        assert!(r.range(T::SegIdx, &[2], Some(&[3])).unwrap().is_empty());
        assert_eq!(r.last(T::SegIdx).unwrap().unwrap().1, vec![4]);
        assert!(r.is_empty(T::Aux).unwrap());
        drop(r);
        drop(env);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn safety_pair_survives_reopen() {
        let d = tmp("safety");
        let mut s = MdbxSafety::open(&d).unwrap();
        assert_eq!(s.load().unwrap(), None);
        assert_eq!(s.persist_intent(b"i1", b"r1").unwrap().seq, 1);
        assert_eq!(s.persist_intent(b"i2", b"rec2").unwrap().seq, 2);
        drop(s);
        let mut s = MdbxSafety::open(&d).unwrap();
        let l = s.load().unwrap().unwrap();
        assert_eq!(
            (l.intent.as_slice(), l.record.as_slice()),
            (&b"i2"[..], &b"rec2"[..])
        );
        assert_eq!(s.persist_intent(b"i3", b"").unwrap().seq, 3);
        let _ = fs::remove_dir_all(&d);
    }
}
