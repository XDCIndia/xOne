//! `--gcmode archive`: the state after any canonical block, read from the history tables over the
//! head's tables in one MDBX read transaction (issue #152).
//!
//! Every block commit of an archive datadir writes its changeset twice: by block
//! (`AccountChangeSets`, for the unwind) and by state key (`AccountHistory`, `StorageHistory`). A
//! history row `key || m` holds the value `key` had before block `m` changed it. The value after
//! block `n` is therefore the first row of `key` above `n` (no block between `n` and `m` changed it),
//! and the head's row when there is none: one cursor seek per read, as reth's and erigon's history
//! indexes, with the value in the row instead of a block list.

use std::cell::RefCell;

use alloy_primitives::{keccak256, Address, B256, U256};
use xone_db::{
    mdbx::{prefix_end, MdbxError, Rd, T},
    segments::SegDir,
};
use xone_state::{trie::root_of_sorted_rows, Account, StateRead};

use super::{be, codec::dec_account, StoreReader, GCMODE_KEY, R};
use crate::store::StoreError;

/// The state after block `n`, over one read transaction. A read error or a row that does not decode
/// is kept and reported by [`StoreReader::with_state_at`]; the read itself answers absent.
pub struct HistState<'r, 'e> {
    rd: &'r Rd<'e>,
    above: [u8; 8],
    bad: RefCell<Option<String>>,
}

impl<'r, 'e> HistState<'r, 'e> {
    pub fn new(rd: &'r Rd<'e>, n: u64) -> Self {
        Self {
            rd,
            above: be(n.saturating_add(1)),
            bad: RefCell::new(None),
        }
    }

    /// The first history row of `key` above `n`.
    fn older(&self, t: T, key: &[u8]) -> R<Option<Vec<u8>>> {
        let to = prefix_end(key);
        Ok(self
            .rd
            .range_limit(t, &[key, &self.above[..]].concat(), to.as_deref(), 1)?
            .pop()
            .map(|(_, v)| v))
    }

    fn keep<V>(&self, r: Result<Option<V>, String>) -> Option<V> {
        r.unwrap_or_else(|e| {
            self.bad.borrow_mut().get_or_insert(e);
            None
        })
    }

    pub fn failed(&self) -> Option<String> {
        self.bad.borrow().clone()
    }
}

impl StateRead for HistState<'_, '_> {
    fn account(&self, address: Address) -> Option<Account> {
        let ha = keccak256(address);
        let r = (|| {
            let rlp = match self.older(T::AccountHistory, ha.as_slice())? {
                // the changeset encoding: `0` absent, `1 || rlp`
                Some(v) => v.get(1..).filter(|_| v[0] == 1).map(<[u8]>::to_vec),
                None => self.rd.get(T::HashedAccounts, ha.as_slice())?,
            };
            Ok::<_, MdbxError>(rlp)
        })();
        let rlp = self.keep(r.map_err(|e| e.to_string()))?;
        self.keep(
            dec_account(&rlp)
                .map(Some)
                .ok_or(format!("account row of {ha}")),
        )
    }

    fn storage(&self, address: Address, slot: B256) -> U256 {
        let key = [keccak256(address).as_slice(), keccak256(slot).as_slice()].concat();
        let r = (|| {
            Ok::<_, MdbxError>(match self.older(T::StorageHistory, &key)? {
                Some(v) => Some(v),
                None => self.rd.get(T::HashedStorages, &key)?,
            })
        })();
        self.keep(r.map_err(|e| e.to_string()))
            .map_or(U256::ZERO, |v| U256::from_be_slice(&v))
    }
}

/// The rows of `t` (`hashed key || block` to the prior value) laid over `now` (the head's rows of
/// the same keys): the whole state after block `n` as sorted `(key, value)`, absent keys dropped.
fn rows_at(
    rd: &Rd,
    hist: T,
    now: T,
    n: u64,
    absent: impl Fn(&[u8]) -> Option<Vec<u8>>,
) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut rows: std::collections::BTreeMap<Vec<u8>, Option<Vec<u8>>> = rd
        .range(now, &[], None)?
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect();
    let mut done: Option<Vec<u8>> = None;
    for (k, v) in rd.range(hist, &[], None)? {
        let (key, m) = k.split_at(k.len() - 8);
        // rows of a key ascend by block: the first one above `n` is the value at `n`
        if done.as_deref() == Some(key) || super::be_u64(m).is_none_or(|m| m <= n) {
            continue;
        }
        done = Some(key.to_vec());
        rows.insert(key.to_vec(), absent(&v));
    }
    Ok(rows
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect())
}

/// The state root after block `n`, from every row (`ponytail:` a scan of the state and history
/// tables, for checks and tests; a historical trie walk replaces it if an RPC ever needs it).
pub fn root_at(rd: &Rd, n: u64) -> Result<B256, StoreError> {
    let db = |e: MdbxError| StoreError::Damaged(e.to_string());
    let bad = || StoreError::Damaged(format!("history rows at block {n}"));
    let accounts = rows_at(rd, T::AccountHistory, T::HashedAccounts, n, |v| {
        v.get(1..).filter(|_| v[0] == 1).map(<[u8]>::to_vec)
    })
    .map_err(db)?
    .into_iter()
    .map(|(k, v)| Some((B256::try_from(&k[..]).ok()?, dec_account(&v)?)))
    .collect::<Option<Vec<_>>>()
    .ok_or_else(bad)?;
    let storage = rows_at(rd, T::StorageHistory, T::HashedStorages, n, |v| {
        Some(v.to_vec()).filter(|v| v.iter().any(|b| *b != 0))
    })
    .map_err(db)?
    .into_iter()
    .map(|(k, v)| {
        Some((
            B256::try_from(k.get(..32)?).ok()?,
            B256::try_from(k.get(32..)?).ok()?,
            U256::from_be_slice(&v),
        ))
    })
    .collect::<Option<Vec<_>>>()
    .ok_or_else(bad)?;
    Ok(root_of_sorted_rows(&accounts, &storage))
}

impl<D: SegDir> StoreReader<D> {
    /// Runs `f` on the state after canonical block `n`, which must be `hash`, of an archive datadir.
    /// `Ok(false)`: the datadir is not an archive or `hash` is not canonical block `n`; nothing ran.
    pub fn with_state_at(
        &self,
        n: u64,
        hash: B256,
        f: &mut dyn FnMut(&dyn StateRead),
    ) -> Result<bool, StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.env.read().map_err(db)?;
        let archive = rd.get(T::Meta, GCMODE_KEY).map_err(db)?.as_deref() == Some(b"archive");
        let canonical =
            rd.get(T::CanonicalHeaders, &be(n)).map_err(db)?.as_deref() == Some(hash.as_slice());
        if !(archive && canonical) {
            return Ok(false);
        }
        let s = HistState::new(&rd, n);
        f(&s);
        match s.failed() {
            Some(e) => Err(StoreError::Damaged(e)),
            None => Ok(true),
        }
    }

    /// The rows of `AccountHistory` and `StorageHistory` (from the table metadata).
    pub fn history_rows(&self) -> Result<(usize, usize), StoreError> {
        let db = |e: MdbxError| StoreError::Damaged(e.to_string());
        let rd = self.env.read().map_err(db)?;
        Ok((
            rd.entries(T::AccountHistory).map_err(db)?,
            rd.entries(T::StorageHistory).map_err(db)?,
        ))
    }

    /// The state root after block `n` of an archive datadir, from every row ([`root_at`]).
    pub fn state_root_at(&self, n: u64) -> Result<B256, StoreError> {
        let rd = self
            .env
            .read()
            .map_err(|e| StoreError::Damaged(e.to_string()))?;
        root_at(&rd, n)
    }
}
