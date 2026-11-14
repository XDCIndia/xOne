//! The queue of nonce-gapped transactions and the virtual nonces of the pool: geth `queue`
//! (`queue.go`) and `noncer` (`noncer.go`).

use super::{
    list::{Add, List, Tx},
    Error, Pool,
};
use alloy_primitives::{Address, B256};
use std::collections::BTreeMap;
use xone_state::StateRead;

/// The pool's own view of each account's next nonce, falling back to the head state.
#[derive(Default)]
pub(crate) struct Noncer {
    nonces: BTreeMap<Address, u64>,
}

impl Noncer {
    fn load<S: StateRead>(&mut self, state: &S, a: Address) {
        if let std::collections::btree_map::Entry::Vacant(e) = self.nonces.entry(a) {
            if let Some(n) = state.account(a).map(|x| x.nonce).filter(|n| *n != 0) {
                e.insert(n);
            }
        }
    }
    pub(crate) fn get<S: StateRead>(&mut self, state: &S, a: Address) -> u64 {
        self.load(state, a);
        self.nonces.get(&a).copied().unwrap_or(0)
    }
    pub(crate) fn set(&mut self, a: Address, n: u64) {
        self.nonces.insert(a, n);
    }
    pub(crate) fn set_if_lower<S: StateRead>(&mut self, state: &S, a: Address, n: u64) {
        self.load(state, a);
        if self.nonces.get(&a).copied().unwrap_or(0) <= n {
            return;
        }
        self.nonces.insert(a, n);
    }
    pub(crate) fn set_if_higher<S: StateRead>(&mut self, state: &S, a: Address, n: u64) {
        self.load(state, a);
        if self.nonces.get(&a).copied().unwrap_or(0) >= n {
            return;
        }
        self.nonces.insert(a, n);
    }
    pub(crate) fn set_all(&mut self, all: BTreeMap<Address, u64>) {
        self.nonces = all;
    }
}

/// An account's last activity. `seq` orders two beats stamped in the same second the way a real
/// clock orders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Beat {
    pub(crate) at: u64,
    pub(crate) seq: u64,
}

#[derive(Default)]
pub(crate) struct Queue {
    queued: BTreeMap<Address, List>,
    beats: BTreeMap<Address, Beat>,
    seq: u64,
}

impl Queue {
    fn stamp(&mut self, now: u64) -> Beat {
        self.seq += 1;
        Beat {
            at: now,
            seq: self.seq,
        }
    }

    pub(crate) fn stats(&self) -> usize {
        self.queued.values().map(List::len).sum()
    }

    pub(crate) fn content(&self) -> BTreeMap<Address, Vec<Tx>> {
        self.queued.iter().map(|(a, l)| (*a, l.flatten())).collect()
    }

    pub(crate) fn get(&self, a: &Address) -> Option<&List> {
        self.queued.get(a)
    }

    pub(crate) fn addresses(&self) -> Vec<Address> {
        self.queued.keys().copied().collect()
    }

    /// `bump`: a no-op for an account the queue does not know.
    pub(crate) fn bump(&mut self, a: Address, now: u64) {
        if self.beats.contains_key(&a) {
            let b = self.stamp(now);
            self.beats.insert(a, b);
        }
    }

    /// Test hook of the oracle scenarios: the heartbeat of a queued account, `age` seconds ago.
    pub(crate) fn set_age(&mut self, a: Address, now: u64, age: u64) {
        if self.beats.contains_key(&a) {
            let mut b = self.stamp(now);
            b.at = now.saturating_sub(age);
            self.beats.insert(a, b);
        }
    }

    /// `evictList`: the hashes of every account idle for more than `lifetime`.
    pub(crate) fn evict_list(&self, now: u64, lifetime: u64) -> Vec<B256> {
        let mut out = Vec::new();
        for (a, l) in &self.queued {
            let idle = self.beats.get(a).map_or(0, |b| now.saturating_sub(b.at));
            if idle > lifetime {
                out.extend(l.flatten().iter().map(|t| t.hash));
            }
        }
        out
    }

    pub(crate) fn remove(&mut self, a: Address, tx: &Tx) {
        let Some(l) = self.queued.get_mut(&a) else {
            return;
        };
        // A different transaction at the same nonce is left alone (`queue.go:108-112`).
        if l.get(tx.tx.nonce).is_some_and(|o| o.hash != tx.hash) {
            return;
        }
        l.remove(tx);
        if l.is_empty() {
            self.queued.remove(&a);
            self.beats.remove(&a);
        }
    }

    /// `queue.add`: the replaced transaction's hash, if any.
    pub(crate) fn add(&mut self, tx: &Tx, bump: u64, now: u64) -> Result<Option<B256>, Error> {
        let from = tx.from();
        let l = self.queued.entry(from).or_insert_with(|| List::new(false));
        match l.add(tx, bump) {
            Add::Rejected => {
                // geth leaves an empty list behind here; it only exists when the insert was the first.
                if l.is_empty() {
                    self.queued.remove(&from);
                }
                Err(Error::ReplaceUnderpriced)
            }
            Add::Inserted(old) => {
                if !self.beats.contains_key(&from) {
                    let b = self.stamp(now);
                    self.beats.insert(from, b);
                }
                Ok(old.map(|o| o.hash))
            }
        }
    }

    /// `queue.promoteExecutables`, `queue.go:167-226`. Returns the transactions to promote, the hashes
    /// dropped and the accounts the queue lost.
    pub(crate) fn promote_executables<S: StateRead>(
        &mut self,
        accounts: &[Address],
        gas_limit: u64,
        state: &S,
        nonces: &mut Noncer,
        account_queue: usize,
        trc21: &super::Trc21Fees,
    ) -> (Vec<Tx>, Vec<B256>, Vec<Address>) {
        let (mut promotable, mut dropped, mut removed) = (Vec::new(), Vec::new(), Vec::new());
        for a in accounts {
            let Some(l) = self.queued.get_mut(a) else {
                continue;
            };
            let acct = state.account(*a);
            let (nonce, bal) = acct
                .as_ref()
                .map_or((0, Default::default()), |x| (x.nonce, x.balance.low()));
            dropped.extend(l.forward(nonce).iter().map(|t| t.hash));
            let (drops, _) = l.filter(bal, gas_limit, trc21);
            dropped.extend(drops.iter().map(|t| t.hash));
            promotable.extend(l.ready(nonces.get(state, *a)));
            dropped.extend(l.cap(account_queue).iter().map(|t| t.hash));
            if l.is_empty() {
                self.queued.remove(a);
                self.beats.remove(a);
                removed.push(*a);
            }
        }
        (promotable, dropped, removed)
    }

    /// `queue.truncate`, `queue.go:228-284`: while the queue is over `global`, drop the transactions of
    /// the account idle the longest (all of them if they fit in the overflow, else its highest nonces).
    pub(crate) fn truncate(&mut self, global: usize) -> Vec<B256> {
        let queued = self.stats();
        if queued <= global {
            return Vec::new();
        }
        let mut by_beat: Vec<(Beat, Address)> = self
            .queued
            .keys()
            .map(|a| {
                (
                    self.beats.get(a).copied().unwrap_or(Beat { at: 0, seq: 0 }),
                    *a,
                )
            })
            .collect();
        by_beat.sort();
        let mut removed = Vec::new();
        let mut drop = queued - global;
        for (_, a) in by_beat {
            if drop == 0 {
                break;
            }
            let Some(l) = self.queued.get(&a) else {
                continue;
            };
            let txs = l.flatten();
            if txs.len() <= drop {
                for t in &txs {
                    self.remove(a, t);
                    removed.push(t.hash);
                }
                drop -= txs.len();
                continue;
            }
            for t in txs.iter().rev() {
                if drop == 0 {
                    break;
                }
                self.remove(a, t);
                removed.push(t.hash);
                drop -= 1;
            }
        }
        removed
    }
}

impl Pool {
    /// Age the heartbeat of a queued account. For the oracle scenarios, whose clock is scripted; not for the node.
    #[doc(hidden)]
    pub fn set_queue_age(&mut self, now: u64, addr: Address, age: u64) {
        self.now = now;
        self.queue.set_age(addr, now, age);
    }

    pub(crate) fn enqueue_tx<S: StateRead>(
        &mut self,
        state: &S,
        tx: &Tx,
        add_all: bool,
    ) -> Result<bool, Error> {
        let replaced = self.queue.add(tx, self.cfg.price_bump, self.now)?;
        if let Some(h) = replaced {
            self.remove_tx(state, h, true);
        }
        if add_all {
            self.all.add(tx);
            self.priced.put(tx);
        }
        Ok(replaced.is_some())
    }

    /// `promoteExecutables`, `legacypool.go:1888-1919`.
    pub(crate) fn promote_executables<S: StateRead>(
        &mut self,
        state: &S,
        accounts: &[Address],
    ) -> Vec<Tx> {
        let (promotable, dropped, _) = self.queue.promote_executables(
            accounts,
            self.head.gas_limit,
            state,
            &mut self.nonces,
            usize::try_from(self.cfg.account_queue).unwrap_or(usize::MAX),
            &self.trc21,
        );
        let mut promoted = Vec::new();
        for tx in promotable {
            if self.promote_tx(tx.from(), &tx) {
                promoted.push(tx);
            }
        }
        for h in &dropped {
            self.all.remove(h);
        }
        self.priced.removed(dropped.len(), &self.all);
        promoted
    }

    /// `queue.truncate` and the lookup cleanup after it (`truncateQueue`, `legacypool.go:2001-2018`).
    pub(crate) fn truncate_queue(&mut self) {
        let removed = self
            .queue
            .truncate(usize::try_from(self.cfg.global_queue).unwrap_or(usize::MAX));
        for h in &removed {
            self.all.remove(h);
        }
        self.priced.removed(removed.len(), &self.all);
    }
}
