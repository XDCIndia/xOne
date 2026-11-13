//! [`SnapState`]: the snap/1 download kept in the store's own state tables (`--state.disk`).
//!
//! The rows the client proves go straight into `HashedAccounts`, `HashedStorages` and `Bytecodes` of
//! `chain.mdbx`, and the cursors into `SnapProgress`, each write in one transaction with the cursor it
//! belongs to. A run that dies resumes from them: a stored range is not asked for again, and the
//! pivot of the next run goes through `set_root`, as a pivot that moved does. Nothing holds the
//! state in memory except the accounts for one heal walk (they are hashed into a trie there) and the
//! slots of one account whose storage trie is healed.
//!
//! The install is then not a copy: [`crate::store::Install::rows_in_tables`] says the rows are
//! where they belong, and `install_snapshot` checks the root by streaming them and writes the
//! headers, the manifest and the trie rows.
//!
//! The accounts keep the storage root the proof gave them (the flat state never reads it; the trie
//! rebuild computes the real one from the slots).

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use alloy_primitives::b256;
use alloy_primitives::{Bytes, B256, KECCAK256_EMPTY, U256};
use alloy_rlp::Decodable;
use xone_db::mdbx::{prefix_end, Env, MdbxError, Wr, T};
use xone_snap::client::{Backend, Progress, SlotSet, StoreError};
use xone_state::{
    trie::{state_root_sorted, storage_root_sorted},
    Account,
};

/// The root of an empty trie.
const EMPTY_ROOT_HASH: B256 =
    b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");

type R<V> = Result<V, StoreError>;

const PROGRESS_KEY: &[u8] = b"p";
const PAGE: usize = 20_000;

fn db(e: MdbxError) -> StoreError {
    StoreError(e.to_string())
}

fn skey(ha: &B256) -> Vec<u8> {
    [&[1u8][..], ha.as_slice()].concat()
}

fn enc_progress(p: &Progress) -> Vec<u8> {
    let mut v = p.root.to_vec();
    let flags = u8::from(p.next_account.is_some())
        | u8::from(p.verified.is_some()) << 1
        | u8::from(p.stale) << 2;
    v.push(flags);
    if let Some(n) = p.next_account {
        v.extend_from_slice(n.as_slice());
    }
    if let Some(n) = p.verified {
        v.extend_from_slice(n.as_slice());
    }
    v
}

fn dec_progress(b: &[u8]) -> Option<Progress> {
    let root = B256::try_from(b.get(..32)?).ok()?;
    let flags = *b.get(32)?;
    let mut at = 33;
    let mut take = |present: bool| -> Option<Option<B256>> {
        if !present {
            return Some(None);
        }
        let h = B256::try_from(b.get(at..at + 32)?).ok()?;
        at += 32;
        Some(Some(h))
    };
    let next_account = take(flags & 1 != 0)?;
    let verified = take(flags & 2 != 0)?;
    Some(Progress {
        root,
        next_account,
        storage_next: BTreeMap::new(),
        verified,
        stale: flags & 4 != 0,
    })
}

fn dec_account(v: &[u8]) -> R<Account> {
    Account::decode(&mut &v[..]).map_err(|e| StoreError(format!("account row: {e}")))
}

/// Rows of `t` from `from` below `to`, one page per read transaction. A failed read ends the scan and
/// sets `failed`.
struct Scan<'a> {
    env: &'a Env,
    t: T,
    next: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
    page: std::collections::VecDeque<(Vec<u8>, Vec<u8>)>,
    failed: Rc<Cell<bool>>,
}

impl<'a> Scan<'a> {
    fn new(
        env: &'a Env,
        t: T,
        from: Vec<u8>,
        end: Option<Vec<u8>>,
        failed: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            env,
            t,
            next: Some(from),
            end,
            page: Default::default(),
            failed,
        }
    }
}

impl Iterator for Scan<'_> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.page.is_empty() {
            let from = self.next.take()?;
            let rows = match self
                .env
                .read()
                .and_then(|rd| rd.range_limit(self.t, &from, self.end.as_deref(), PAGE))
            {
                Ok(r) => r,
                Err(_) => {
                    self.failed.set(true);
                    return None;
                }
            };
            if rows.len() == PAGE {
                self.next = rows.last().map(|(k, _)| [k.as_slice(), &[0]].concat());
            }
            self.page = rows.into();
        }
        self.page.pop_front()
    }
}

pub struct SnapState {
    env: Arc<Env>,
    progress: Progress,
    /// Accounts whose storage is partly downloaded, with the origin to go on from.
    partial: BTreeMap<B256, B256>,
    /// Accounts below it all have their storage started or done (a scan for the next batch starts here).
    cursor: B256,
}

impl SnapState {
    /// The download at `root` in the tables of `env`: the cursors of an earlier run, with the pivot
    /// replaced when it was another one (what a pivot that moved does), else a new one.
    pub fn open(env: Arc<Env>, root: B256) -> R<Self> {
        let rd = env.read().map_err(db)?;
        let row = rd.get(T::SnapProgress, PROGRESS_KEY).map_err(db)?;
        let held = !rd.is_empty(T::HashedAccounts).map_err(db)?;
        let mut progress = match row.as_deref().map(dec_progress) {
            Some(Some(p)) => p,
            Some(None) => return Err(StoreError("snap progress row is damaged".into())),
            None => Progress {
                root,
                next_account: (root != EMPTY_ROOT_HASH).then_some(B256::ZERO),
                storage_next: BTreeMap::new(),
                verified: None,
                stale: false,
            },
        };
        let mut partial = BTreeMap::new();
        for (k, v) in rd.prefix(T::SnapProgress, &[1]).map_err(db)? {
            let ha = B256::try_from(
                k.get(1..33)
                    .ok_or_else(|| StoreError("cursor key".into()))?,
            )
            .map_err(|_| StoreError("cursor key".into()))?;
            let next = match v.split_first() {
                Some((0, _)) => None,
                Some((1, o)) => {
                    Some(B256::try_from(o).map_err(|_| StoreError("storage cursor".into()))?)
                }
                _ => return Err(StoreError("storage cursor".into())),
            };
            if let Some(o) = next {
                partial.insert(ha, o);
            }
            progress.storage_next.insert(ha, next);
        }
        drop(rd);
        let mut s = Self {
            env,
            progress,
            partial,
            cursor: B256::ZERO,
        };
        if s.progress.root != root {
            s.set_root(root)?;
        } else if row.is_none() && held {
            // rows without cursors: not a download of ours
            return Err(StoreError(
                "the state tables hold rows but no snap progress".into(),
            ));
        }
        Ok(s)
    }

    /// The rows of the download are the state: it is over (called once the install committed).
    pub fn clear_progress(env: &Env) -> Result<(), MdbxError> {
        let tx = env.write()?;
        tx.clear(T::SnapProgress)?;
        tx.commit()
    }

    fn txn(&self, f: impl FnOnce(&Wr) -> Result<(), MdbxError>) -> R<()> {
        let tx = self.env.write().map_err(db)?;
        f(&tx).map_err(db)?;
        tx.commit().map_err(db)
    }

    fn write_progress(&self, tx: &Wr, p: &Progress) -> Result<(), MdbxError> {
        tx.put(T::SnapProgress, PROGRESS_KEY, &enc_progress(p))
    }

    fn scan(&self, t: T, from: Vec<u8>, end: Option<Vec<u8>>, failed: &Rc<Cell<bool>>) -> Scan<'_> {
        Scan::new(&self.env, t, from, end, failed.clone())
    }

    fn account_rows(&self, failed: &Rc<Cell<bool>>) -> impl Iterator<Item = (B256, Account)> + '_ {
        let f = failed.clone();
        self.scan(T::HashedAccounts, Vec::new(), None, failed)
            .filter_map(move |(k, v)| {
                let ha = B256::try_from(k.as_slice()).ok()?;
                match Account::decode(&mut &v[..]) {
                    Ok(a) => Some((ha, a)),
                    Err(_) => {
                        f.set(true);
                        None
                    }
                }
            })
    }

    fn account(&self, ha: &B256) -> R<Option<Account>> {
        let rd = self.env.read().map_err(db)?;
        rd.get(T::HashedAccounts, ha.as_slice())
            .map_err(db)?
            .map(|v| dec_account(&v))
            .transpose()
    }

    /// Deletes the slots of `ha`, in transactions of at most 100,000 rows.
    fn delete_slots(&self, ha: &B256) -> R<()> {
        let end = prefix_end(ha.as_slice());
        loop {
            let tx = self.env.write().map_err(db)?;
            let rows = tx
                .range_limit(T::HashedStorages, ha.as_slice(), end.as_deref(), 100_000)
                .map_err(db)?;
            if rows.is_empty() {
                return Ok(());
            }
            for (k, _) in rows {
                tx.del(T::HashedStorages, &k).map_err(db)?;
            }
            tx.commit().map_err(db)?;
        }
    }

    fn drop_account(&mut self, ha: &B256) -> R<()> {
        self.delete_slots(ha)?;
        self.partial.remove(ha);
        self.progress.storage_next.remove(ha);
        self.txn(|tx| {
            tx.del(T::HashedAccounts, ha.as_slice())?;
            tx.del(T::SnapProgress, &skey(ha))
        })
    }

    fn forget_storage(&mut self, ha: &B256) -> R<()> {
        self.delete_slots(ha)?;
        self.partial.remove(ha);
        self.progress.storage_next.remove(ha);
        self.txn(|tx| tx.del(T::SnapProgress, &skey(ha)))
    }

    /// Rows of the whole state: accounts and slots, for [`Backend::root`] and the install check.
    pub fn state_root_of(env: &Env) -> R<B256> {
        let failed = Rc::new(Cell::new(false));
        let accounts = Scan::new(env, T::HashedAccounts, Vec::new(), None, failed.clone())
            .filter_map(|(k, v)| {
                Some((
                    B256::try_from(k.as_slice()).ok()?,
                    Account::decode(&mut &v[..]).ok()?,
                ))
            });
        let storage = Scan::new(env, T::HashedStorages, Vec::new(), None, failed.clone())
            .filter_map(|(k, v)| {
                (v.len() <= 32).then(|| {
                    Some((
                        B256::try_from(k.get(..32)?).ok()?,
                        B256::try_from(k.get(32..64)?).ok()?,
                        U256::from_be_slice(&v),
                    ))
                })?
            });
        let root = state_root_sorted(accounts, storage);
        if failed.get() {
            return Err(StoreError("a read of the state tables failed".into()));
        }
        Ok(root)
    }

    /// The accounts whose balance is above 2^256 (their addresses are asked for at the install).
    pub fn wide_accounts(&self) -> R<Vec<B256>> {
        let failed = Rc::new(Cell::new(false));
        let out = self
            .account_rows(&failed)
            .filter(|(_, a)| a.balance.is_wide())
            .map(|(h, _)| h)
            .collect();
        if failed.get() {
            return Err(StoreError("a read of the accounts failed".into()));
        }
        Ok(out)
    }

    /// Every code held.
    pub fn codes(&self) -> R<BTreeMap<B256, Bytes>> {
        let failed = Rc::new(Cell::new(false));
        let out = self
            .scan(T::Bytecodes, Vec::new(), None, &failed)
            .filter_map(|(k, v)| Some((B256::try_from(k.as_slice()).ok()?, Bytes::from(v))))
            .collect();
        if failed.get() {
            return Err(StoreError("a read of the codes failed".into()));
        }
        Ok(out)
    }

    /// Rows held: accounts, slots, codes.
    pub fn counts(&self) -> R<(usize, usize, usize)> {
        let rd = self.env.read().map_err(db)?;
        Ok((
            rd.entries(T::HashedAccounts).map_err(db)?,
            rd.entries(T::HashedStorages).map_err(db)?,
            rd.entries(T::Bytecodes).map_err(db)?,
        ))
    }
}

impl Backend for SnapState {
    fn progress(&self) -> &Progress {
        &self.progress
    }

    fn summary(&self) -> String {
        match self.counts() {
            Ok((a, s, c)) => format!("{a} accounts, {s} slots, {c} codes"),
            Err(e) => format!("(unreadable: {e})"),
        }
    }

    fn set_root(&mut self, root: B256) -> R<()> {
        let held = {
            let rd = self.env.read().map_err(db)?;
            !rd.is_empty(T::HashedAccounts).map_err(db)?
        };
        let mut p = self.progress.clone();
        p.root = root;
        p.verified = None;
        p.stale = held;
        if root == EMPTY_ROOT_HASH {
            self.txn(|tx| {
                tx.clear(T::HashedAccounts)?;
                tx.clear(T::HashedStorages)?;
                tx.clear(T::SnapProgress)
            })?;
            p = Progress {
                root,
                next_account: None,
                storage_next: BTreeMap::new(),
                verified: None,
                stale: false,
            };
            self.partial.clear();
        }
        self.txn(|tx| self.write_progress(tx, &p))?;
        self.progress = p;
        self.cursor = B256::ZERO;
        Ok(())
    }

    fn put_accounts(&mut self, accounts: Vec<(B256, Account)>, next: Option<B256>) -> R<()> {
        let mut p = self.progress.clone();
        p.next_account = next;
        self.txn(|tx| {
            for (h, a) in &accounts {
                tx.put(T::HashedAccounts, h.as_slice(), &a.rlp())?;
            }
            self.write_progress(tx, &p)
        })?;
        self.progress.next_account = next;
        Ok(())
    }

    fn put_slots(&mut self, sets: Vec<SlotSet>) -> R<()> {
        self.txn(|tx| {
            for (acct, slots, next) in &sets {
                for (hs, v) in slots {
                    tx.put(
                        T::HashedStorages,
                        &[acct.as_slice(), hs.as_slice()].concat(),
                        &v.to_be_bytes::<32>(),
                    )?;
                }
                let row = match next {
                    Some(o) => [&[1u8][..], o.as_slice()].concat(),
                    None => vec![0],
                };
                tx.put(T::SnapProgress, &skey(acct), &row)?;
            }
            Ok(())
        })?;
        for (acct, _, next) in sets {
            self.progress.storage_next.insert(acct, next);
            match next {
                Some(o) => {
                    self.partial.insert(acct, o);
                }
                None => {
                    self.partial.remove(&acct);
                }
            }
        }
        Ok(())
    }

    fn put_codes(&mut self, codes: Vec<(B256, Bytes)>) -> R<()> {
        self.txn(|tx| {
            for (h, c) in &codes {
                tx.put(T::Bytecodes, h.as_slice(), c)?;
            }
            Ok(())
        })
    }

    fn storage_batch(&mut self, max: usize) -> R<(B256, Vec<(B256, B256)>)> {
        if let Some((a, o)) = self.partial.iter().next() {
            let acc = self
                .account(a)?
                .ok_or_else(|| StoreError("a storage cursor of an account that is gone".into()))?;
            return Ok((*o, vec![(*a, acc.storage_root)]));
        }
        let failed = Rc::new(Cell::new(false));
        let mut batch = Vec::new();
        let mut last = None;
        for (ha, acc) in self
            .scan(T::HashedAccounts, self.cursor.to_vec(), None, &failed)
            .filter_map(|(k, v)| {
                Some((
                    B256::try_from(k.as_slice()).ok()?,
                    Account::decode(&mut &v[..]).ok()?,
                ))
            })
        {
            last = Some(ha);
            if acc.storage_root == EMPTY_ROOT_HASH || self.progress.storage_next.contains_key(&ha) {
                continue;
            }
            batch.push((ha, acc.storage_root));
            if batch.len() >= max {
                break;
            }
        }
        if failed.get() {
            return Err(StoreError("a read of the accounts failed".into()));
        }
        if batch.len() >= max {
            if let Some(l) = last {
                self.cursor = l;
            }
        }
        Ok((B256::ZERO, batch))
    }

    fn missing_codes(&self) -> R<Vec<B256>> {
        let failed = Rc::new(Cell::new(false));
        let mut seen = HashSet::new();
        let want: Vec<B256> = self
            .account_rows(&failed)
            .map(|(_, a)| a.code_hash)
            .filter(|h| *h != KECCAK256_EMPTY && seen.insert(*h))
            .collect();
        if failed.get() {
            return Err(StoreError("a read of the accounts failed".into()));
        }
        let have = self.has_codes(&want)?;
        Ok(want
            .into_iter()
            .zip(have)
            .filter(|(_, h)| !h)
            .map(|(c, _)| c)
            .collect())
    }

    fn has_codes(&self, hashes: &[B256]) -> R<Vec<bool>> {
        let rd = self.env.read().map_err(db)?;
        hashes
            .iter()
            .map(|h| {
                rd.get(T::Bytecodes, h.as_slice())
                    .map(|v| v.is_some())
                    .map_err(db)
            })
            .collect()
    }

    fn accounts(&self) -> R<BTreeMap<B256, Account>> {
        let failed = Rc::new(Cell::new(false));
        let out = self.account_rows(&failed).collect();
        if failed.get() {
            return Err(StoreError("a read of the accounts failed".into()));
        }
        Ok(out)
    }

    fn slots_root(&self, account: &B256) -> R<Option<B256>> {
        let failed = Rc::new(Cell::new(false));
        let mut any = false;
        let root = storage_root_sorted(
            self.scan(
                T::HashedStorages,
                account.to_vec(),
                prefix_end(account.as_slice()),
                &failed,
            )
            .filter_map(|(k, v)| {
                (v.len() <= 32).then(|| {
                    Some((
                        B256::try_from(k.get(32..64)?).ok()?,
                        U256::from_be_slice(&v),
                    ))
                })?
            })
            .inspect(|_| any = true),
        );
        if failed.get() {
            return Err(StoreError("a read of the slots failed".into()));
        }
        Ok(any.then_some(root))
    }

    fn slots(&self, account: &B256) -> R<BTreeMap<B256, U256>> {
        let failed = Rc::new(Cell::new(false));
        let out = self
            .scan(
                T::HashedStorages,
                account.to_vec(),
                prefix_end(account.as_slice()),
                &failed,
            )
            .filter_map(|(k, v)| {
                (v.len() <= 32).then(|| {
                    Some((
                        B256::try_from(k.get(32..64)?).ok()?,
                        U256::from_be_slice(&v),
                    ))
                })?
            })
            .collect();
        if failed.get() {
            return Err(StoreError("a read of the slots failed".into()));
        }
        Ok(out)
    }

    fn reconcile(&mut self, new: BTreeMap<B256, Account>) -> R<()> {
        let old = self.accounts()?;
        for (h, o) in &old {
            match new.get(h) {
                None => self.drop_account(h)?,
                Some(a) if a.storage_root != o.storage_root => self.forget_storage(h)?,
                Some(_) => {}
            }
        }
        let mut p = self.progress.clone();
        p.stale = false;
        self.txn(|tx| {
            for (h, a) in &new {
                tx.put(T::HashedAccounts, h.as_slice(), &a.rlp())?;
            }
            self.write_progress(tx, &p)
        })?;
        self.progress.stale = false;
        self.cursor = B256::ZERO;
        Ok(())
    }

    fn heal(
        &mut self,
        accounts: BTreeMap<B256, Account>,
        storage: BTreeMap<B256, BTreeMap<B256, U256>>,
    ) -> R<()> {
        let old = self.accounts()?;
        for h in old.keys().filter(|h| !accounts.contains_key(*h)) {
            self.drop_account(h)?;
        }
        self.txn(|tx| {
            for (h, a) in &accounts {
                tx.put(T::HashedAccounts, h.as_slice(), &a.rlp())?;
            }
            Ok(())
        })?;
        for (h, slots) in &storage {
            self.delete_slots(h)?;
            for chunk in slots.iter().collect::<Vec<_>>().chunks(100_000) {
                self.txn(|tx| {
                    for (hs, v) in chunk {
                        tx.put(
                            T::HashedStorages,
                            &[h.as_slice(), hs.as_slice()].concat(),
                            &v.to_be_bytes::<32>(),
                        )?;
                    }
                    Ok(())
                })?;
            }
        }
        // an account without storage at the root keeps none
        for (h, a) in &accounts {
            if a.storage_root == EMPTY_ROOT_HASH {
                self.delete_slots(h)?;
            }
        }
        self.finish_accounts()
    }

    fn finish_accounts(&mut self) -> R<()> {
        let with_storage: Vec<B256> = self
            .accounts()?
            .into_iter()
            .filter(|(_, a)| a.storage_root != EMPTY_ROOT_HASH)
            .map(|(h, _)| h)
            .collect();
        let mut p = self.progress.clone();
        p.next_account = None;
        self.txn(|tx| {
            tx.clear(T::SnapProgress)?;
            for h in &with_storage {
                tx.put(T::SnapProgress, &skey(h), &[0])?;
            }
            self.write_progress(tx, &p)
        })?;
        p.storage_next = with_storage.into_iter().map(|h| (h, None)).collect();
        self.progress = p;
        self.partial.clear();
        self.cursor = B256::ZERO;
        Ok(())
    }

    fn root(&self) -> R<B256> {
        Self::state_root_of(&self.env)
    }

    fn set_verified(&mut self, root: B256) -> R<()> {
        let mut p = self.progress.clone();
        p.verified = Some(root);
        self.txn(|tx| self.write_progress(tx, &p))?;
        self.progress = p;
        Ok(())
    }
}
