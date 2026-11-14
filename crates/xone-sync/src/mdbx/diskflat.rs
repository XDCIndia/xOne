//! [`DiskFlat`]: the flat account and storage state read from the MDBX tables `HashedAccounts` and
//! `HashedStorages` behind two bounded caches (`--state.disk`).
//!
//! The tables are the state. The store writes them in the block's own transaction; this view never
//! writes a row. After the commit the block's diff is written through to the caches
//! ([`FlatWrite::commit_written`]), after an unwind the changeset is ([`FlatWrite::set_hashed_account`]
//! and `set_hashed_slot` only touch the caches), so a cache entry is always what the table holds.
//! A cache entry may say "absent" (`None`, zero).
//!
//! A cache is two generations of a map: a read finds an entry in the young or the old one (and
//! moves it to the young); when the young one is full the old one is dropped and the young becomes
//! old. That is LRU within a factor of two, with no per-entry bookkeeping.
//!
//! The scans ([`FlatRead::hashed_accounts`], [`FlatRead::hashed_slots`]) read pages of [`PAGE`] rows
//! in short read transactions, so the iterator holds no transaction and no copy of the table.
//!
//! A read that fails (a damaged page, a full disk) cannot be reported through [`FlatRead`]: it
//! answers "absent" and sets [`DiskFlat::failed`], which the store checks before it commits and
//! after, and then stops. A block executed on a wrong answer never matches its header's state root.

use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    mem,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, PoisonError,
    },
};

use alloy_primitives::{B256, U256};
use xone_db::mdbx::{prefix_end, Env, T};
use xone_state::{Account, ChangeSet, FlatRead, FlatWrite, HashedDiff, MemFlat};

use super::codec::dec_account;

/// Rows per read transaction of a scan.
pub const PAGE: usize = 20_000;

/// Entries of the young generation of the account cache and of the slot cache (the cache holds up
/// to twice as many).
pub const ACCOUNTS_GEN: usize = 100_000;
pub const SLOTS_GEN: usize = 500_000;

struct Gen<K, V> {
    young: HashMap<K, V>,
    old: HashMap<K, V>,
    cap: usize,
}

impl<K: Hash + Eq + Copy, V: Clone> Gen<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            young: HashMap::new(),
            old: HashMap::new(),
            cap: cap.max(1),
        }
    }

    fn get(&mut self, k: &K) -> Option<V> {
        if let Some(v) = self.young.get(k) {
            return Some(v.clone());
        }
        let v = self.old.remove(k)?;
        self.put(*k, v.clone());
        Some(v)
    }

    fn put(&mut self, k: K, v: V) {
        self.old.remove(&k);
        self.young.insert(k, v);
        if self.young.len() >= self.cap {
            self.old = mem::take(&mut self.young);
        }
    }

    fn len(&self) -> usize {
        self.young.len() + self.old.len()
    }
}

/// Hit and miss counts, for the node's log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub account_hits: u64,
    pub account_misses: u64,
    pub slot_hits: u64,
    pub slot_misses: u64,
    pub cached: usize,
}

pub struct DiskFlat {
    env: Arc<Env>,
    accounts: Mutex<Gen<B256, Option<Account>>>,
    slots: Mutex<Gen<(B256, B256), U256>>,
    failed: AtomicBool,
    counts: [AtomicU64; 4],
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DiskFlat {
    pub fn new(env: Arc<Env>) -> Self {
        Self::with_caps(env, ACCOUNTS_GEN, SLOTS_GEN)
    }

    /// Caches of `accounts` and `slots` entries per generation (tests use tiny ones, so that nearly
    /// every read goes to the tables).
    pub fn with_caps(env: Arc<Env>, accounts: usize, slots: usize) -> Self {
        Self {
            env,
            accounts: Mutex::new(Gen::new(accounts)),
            slots: Mutex::new(Gen::new(slots)),
            failed: AtomicBool::new(false),
            counts: Default::default(),
        }
    }

    /// A read failed since the last [`Self::clear_failed`]: an answer of this view may be wrong.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn clear_failed(&self) {
        self.failed.store(false, Ordering::Relaxed);
    }

    pub fn stats(&self) -> Stats {
        let c = |i: usize| self.counts[i].load(Ordering::Relaxed);
        Stats {
            account_hits: c(0),
            account_misses: c(1),
            slot_hits: c(2),
            slot_misses: c(3),
            cached: lock(&self.accounts).len() + lock(&self.slots).len(),
        }
    }

    fn count(&self, i: usize) {
        self.counts[i].fetch_add(1, Ordering::Relaxed);
    }

    fn fail(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    fn row(&self, t: T, key: &[u8]) -> Option<Option<Vec<u8>>> {
        match self.env.read().and_then(|rd| rd.get(t, key)) {
            Ok(v) => Some(v),
            Err(_) => {
                self.fail();
                None
            }
        }
    }
}

impl FlatRead for DiskFlat {
    fn hashed_account(&self, ha: B256) -> Option<Account> {
        if let Some(a) = lock(&self.accounts).get(&ha) {
            self.count(0);
            return a;
        }
        self.count(1);
        let acc = match self.row(T::HashedAccounts, ha.as_slice())? {
            None => None,
            Some(r) => match dec_account(&r) {
                Some(a) => Some(a),
                None => {
                    self.fail();
                    return None;
                }
            },
        };
        lock(&self.accounts).put(ha, acc.clone());
        acc
    }

    fn hashed_storage(&self, ha: B256, hs: B256) -> U256 {
        if let Some(v) = lock(&self.slots).get(&(ha, hs)) {
            self.count(2);
            return v;
        }
        self.count(3);
        let key = [ha.as_slice(), hs.as_slice()].concat();
        let v = match self.row(T::HashedStorages, &key) {
            None => return U256::ZERO,
            Some(None) => U256::ZERO,
            Some(Some(r)) if r.len() <= 32 => U256::from_be_slice(&r),
            Some(Some(_)) => {
                self.fail();
                return U256::ZERO;
            }
        };
        lock(&self.slots).put((ha, hs), v);
        v
    }

    fn hashed_accounts(&self) -> Box<dyn Iterator<Item = (B256, Account)> + '_> {
        Box::new(
            Scan::new(self, T::HashedAccounts, Vec::new(), None).filter_map(|(k, v)| {
                let ha = B256::try_from(k.as_slice()).ok()?;
                match dec_account(&v) {
                    Some(a) => Some((ha, a)),
                    None => {
                        self.fail();
                        None
                    }
                }
            }),
        )
    }

    fn hashed_slots(&self, ha: B256) -> Box<dyn Iterator<Item = (B256, U256)> + '_> {
        Box::new(
            Scan::new(
                self,
                T::HashedStorages,
                ha.to_vec(),
                prefix_end(ha.as_slice()),
            )
            .filter_map(|(k, v)| {
                let hs = B256::try_from(k.get(32..)?).ok()?;
                if v.len() > 32 {
                    self.fail();
                    return None;
                }
                Some((hs, U256::from_be_slice(&v)))
            }),
        )
    }
}

/// Rows of `t` from `next` up to `end`, one page per read transaction.
struct Scan<'a> {
    flat: &'a DiskFlat,
    t: T,
    next: Option<Vec<u8>>,
    end: Option<Vec<u8>>,
    page: VecDeque<(Vec<u8>, Vec<u8>)>,
}

impl<'a> Scan<'a> {
    fn new(flat: &'a DiskFlat, t: T, from: Vec<u8>, end: Option<Vec<u8>>) -> Self {
        Self {
            flat,
            t,
            next: Some(from),
            end,
            page: VecDeque::new(),
        }
    }
}

impl Iterator for Scan<'_> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.page.is_empty() {
            let from = self.next.take()?;
            let rows = match self
                .flat
                .env
                .read()
                .and_then(|rd| rd.range_limit(self.t, &from, self.end.as_deref(), PAGE))
            {
                Ok(r) => r,
                Err(_) => {
                    self.flat.fail();
                    return None;
                }
            };
            if rows.len() == PAGE {
                // the key after the last one of the page
                self.next = rows.last().map(|(k, _)| [k.as_slice(), &[0]].concat());
            }
            self.page = rows.into();
        }
        self.page.pop_front()
    }
}

/// Cache writes only: the tables are the store's to write, and it has done so (a block commit) or
/// is about to read them back (an unwind).
impl FlatWrite for DiskFlat {
    fn set_hashed_account(&mut self, ha: B256, account: Option<Account>) {
        lock(&self.accounts).put(ha, account);
    }

    fn set_hashed_slot(&mut self, ha: B256, hs: B256, value: U256) {
        lock(&self.slots).put((ha, hs), value);
    }

    /// Writes the block through to the caches: every account the diff lists, every slot it lists,
    /// and every slot the changeset lists that the diff does not (those of a deleted account, now
    /// zero).
    fn commit_written(&mut self, diff: &HashedDiff, cs: &ChangeSet) {
        let mut slots = lock(&self.slots);
        for &(ha, hs) in cs.storage.keys().chain(diff.storage.keys()) {
            let v = if matches!(diff.accounts.get(&ha), Some(None)) {
                U256::ZERO
            } else {
                diff.storage.get(&(ha, hs)).copied().unwrap_or(U256::ZERO)
            };
            slots.put((ha, hs), v);
        }
        drop(slots);
        let mut accounts = lock(&self.accounts);
        for (ha, new) in &diff.accounts {
            accounts.put(*ha, new.clone());
        }
    }
}

/// The flat state of a store: the whole state in memory, or the tables behind caches.
pub enum Flat {
    Mem(MemFlat),
    Disk(DiskFlat),
}

impl FlatRead for Flat {
    fn hashed_account(&self, ha: B256) -> Option<Account> {
        match self {
            Flat::Mem(f) => f.hashed_account(ha),
            Flat::Disk(f) => f.hashed_account(ha),
        }
    }
    fn hashed_storage(&self, ha: B256, hs: B256) -> U256 {
        match self {
            Flat::Mem(f) => f.hashed_storage(ha, hs),
            Flat::Disk(f) => f.hashed_storage(ha, hs),
        }
    }
    fn hashed_accounts(&self) -> Box<dyn Iterator<Item = (B256, Account)> + '_> {
        match self {
            Flat::Mem(f) => f.hashed_accounts(),
            Flat::Disk(f) => f.hashed_accounts(),
        }
    }
    fn hashed_slots(&self, ha: B256) -> Box<dyn Iterator<Item = (B256, U256)> + '_> {
        match self {
            Flat::Mem(f) => f.hashed_slots(ha),
            Flat::Disk(f) => f.hashed_slots(ha),
        }
    }
}

impl FlatWrite for Flat {
    fn set_hashed_account(&mut self, ha: B256, account: Option<Account>) {
        match self {
            Flat::Mem(f) => f.set_hashed_account(ha, account),
            Flat::Disk(f) => f.set_hashed_account(ha, account),
        }
    }
    fn set_hashed_slot(&mut self, ha: B256, hs: B256, value: U256) {
        match self {
            Flat::Mem(f) => f.set_hashed_slot(ha, hs, value),
            Flat::Disk(f) => f.set_hashed_slot(ha, hs, value),
        }
    }
    fn commit_written(&mut self, diff: &HashedDiff, cs: &ChangeSet) {
        match self {
            Flat::Mem(f) => f.commit_written(diff, cs),
            Flat::Disk(f) => f.commit_written(diff, cs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbx::{
        changeset_of,
        history::{root_at, HistState},
        put_account, put_slot, revert_block, write_diff,
    };
    use alloy_primitives::{keccak256, Address};
    use proptest::prelude::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering as O},
    };
    use xone_primitives::WideBalance;
    use xone_state::{StateDiff, StateRead};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!(
                "xone-diskflat-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, O::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&d);
            Self(d)
        }
        fn env(&self) -> Arc<Env> {
            let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
            Arc::new(Env::open(&self.0, &names).unwrap())
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn acc(nonce: u64) -> Account {
        Account {
            nonce,
            balance: WideBalance::from(U256::from(nonce * 7)),
            ..Account::default()
        }
    }

    fn addr(i: u8) -> Address {
        Address::repeat_byte(i + 1)
    }

    #[derive(Debug, Clone)]
    enum Op {
        Block(Vec<(u8, Option<u64>)>, Vec<(u8, u8, u8)>),
        Unwind,
        Restart,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => (
                prop::collection::vec((0u8..4, prop::option::of(1u64..4)), 0..4),
                prop::collection::vec((0u8..4, 0u8..4, 0u8..3), 0..6),
            )
                .prop_map(|(a, s)| Op::Block(a, s)),
            2 => Just(Op::Unwind),
            1 => Just(Op::Restart),
        ]
    }

    fn same(disk: &DiskFlat, mem: &MemFlat) {
        for i in 0..4 {
            let ha = keccak256(addr(i));
            assert_eq!(
                disk.hashed_account(ha),
                mem.hashed_account(ha),
                "account {i}"
            );
            assert_eq!(
                disk.hashed_slots(ha).collect::<Vec<_>>(),
                mem.hashed_slots(ha).collect::<Vec<_>>(),
                "slots of {i}"
            );
            for j in 0..4 {
                let hs = keccak256(B256::repeat_byte(j + 1));
                assert_eq!(disk.hashed_storage(ha, hs), mem.hashed_storage(ha, hs));
            }
        }
        assert_eq!(
            disk.hashed_accounts().collect::<Vec<_>>(),
            mem.hashed_accounts().collect::<Vec<_>>()
        );
        assert!(!disk.failed());
    }

    /// The disk flat state, driven the way the store drives it (rows first, then the cache), against
    /// a `MemFlat` that applies the same blocks, unwinds and restarts.
    fn run(ops: Vec<Op>, caps: (usize, usize)) {
        let dir = Dir::new();
        let env = dir.env();
        let mut disk = DiskFlat::with_caps(env.clone(), caps.0, caps.1);
        let mut mem = MemFlat::default();
        let mut stack: Vec<(u64, ChangeSet)> = Vec::new();
        // archive: the state after every block of the stack, to check the history rows against
        let mut states = vec![MemFlat::default()];
        for op in ops {
            match op {
                Op::Block(accounts, slots) => {
                    let n = u64::try_from(stack.len()).unwrap() + 1;
                    let mut d = StateDiff::default();
                    for (i, a) in accounts {
                        d.accounts.insert(addr(i), a.map(acc));
                    }
                    for (i, j, v) in slots {
                        d.storage
                            .insert((addr(i), B256::repeat_byte(j + 1)), U256::from(v));
                    }
                    let tx = env.write().unwrap();
                    write_diff(&tx, n, &d, true).unwrap();
                    tx.commit().unwrap();
                    let cs = d.apply(&mut mem).unwrap();
                    let from_rows = changeset_of(&env.read().unwrap(), n).unwrap();
                    assert_eq!(from_rows, cs, "the rows hold the changeset");
                    disk.commit_written(&d.hashed(), &from_rows);
                    stack.push((n, cs));
                    states.push(mem.clone());
                }
                Op::Unwind => {
                    if let Some((n, cs)) = stack.pop() {
                        let tx = env.write().unwrap();
                        revert_block(&tx, n).unwrap();
                        tx.commit().unwrap();
                        cs.revert(&mut mem);
                        cs.revert(&mut disk);
                        states.pop();
                    }
                }
                Op::Restart => disk = DiskFlat::with_caps(env.clone(), caps.0, caps.1),
            }
            same(&disk, &mem);
            history_is_every_state(&env, &stack, &states);
        }
    }

    /// The history rows give the state after every block of the stack, and hold the rows of those
    /// blocks only: an unwind removed the rows of the blocks it popped.
    fn history_is_every_state(env: &Env, stack: &[(u64, ChangeSet)], states: &[MemFlat]) {
        let rd = env.read().unwrap();
        for (k, want) in states.iter().enumerate() {
            let k = u64::try_from(k).unwrap();
            let h = HistState::new(&rd, k);
            for i in 0..4 {
                let ha = keccak256(addr(i));
                assert_eq!(
                    h.account(addr(i)),
                    want.hashed_account(ha),
                    "block {k} account {i}"
                );
                for j in 0..4 {
                    let s = B256::repeat_byte(j + 1);
                    assert_eq!(
                        h.storage(addr(i), s),
                        want.hashed_storage(ha, keccak256(s)),
                        "block {k} slot {i}/{j}"
                    );
                }
            }
            assert!(h.failed().is_none());
            assert_eq!(
                root_at(&rd, k).unwrap(),
                xone_state::trie::flat_root(want),
                "root at {k}"
            );
        }
        let (a, s) = stack.iter().fold((0, 0), |(a, s), (_, cs)| {
            (a + cs.accounts.len(), s + cs.storage.len())
        });
        assert_eq!(rd.entries(T::AccountHistory).unwrap(), a);
        assert_eq!(rd.entries(T::StorageHistory).unwrap(), s);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn disk_flat_equals_mem_flat(ops in prop::collection::vec(op(), 1..24)) {
            run(ops.clone(), (1, 1)); // every read goes to the tables
            run(ops.clone(), (2, 3));
            run(ops, (100, 100)); // everything stays cached
        }
    }

    #[test]
    fn a_scan_pages_through_a_table() {
        let dir = Dir::new();
        let env = dir.env();
        let n = PAGE * 5 / 2 + 7;
        let tx = env.write().unwrap();
        let mut keys = Vec::new();
        for i in 0..n {
            let ha = keccak256(i.to_be_bytes());
            put_account(&tx, &ha, &acc(1).rlp()).unwrap();
            keys.push(ha);
        }
        // one account with more slots than a page, and its neighbours with a few
        keys.sort();
        let big = keys[n / 2];
        for i in 0..PAGE + 3 {
            put_slot(
                &tx,
                &big,
                &keccak256(i.to_be_bytes()),
                &U256::from(1).to_be_bytes::<32>(),
            )
            .unwrap();
        }
        for ha in [keys[n / 2 - 1], keys[n / 2 + 1]] {
            put_slot(
                &tx,
                &ha,
                &B256::repeat_byte(1),
                &U256::from(9).to_be_bytes::<32>(),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let disk = DiskFlat::new(env);
        let got: Vec<B256> = disk.hashed_accounts().map(|(k, _)| k).collect();
        assert_eq!(got, keys);
        let slots: Vec<_> = disk.hashed_slots(big).collect();
        assert_eq!(slots.len(), PAGE + 3);
        assert!(slots.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(disk.hashed_slots(keys[n / 2 - 1]).count(), 1);
        assert_eq!(disk.hashed_slots(keys[n / 2 + 1]).count(), 1);
        assert_eq!(disk.hashed_slots(keys[0]).count(), 0);
        assert!(!disk.failed());
    }
}
