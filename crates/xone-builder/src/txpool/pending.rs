//! The executable lists, and the flows that move transactions between queue and pending: add, reset
//! (promote, demote), removal and truncation. geth `legacypool.go:944-1215`, `:1385-1440`, `:1543-2109`.

use super::{
    list::{Add, List, Tx},
    tier_price, validate, Error, Pool,
};
use alloy_primitives::{Address, B256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use xone_state::StateRead;

/// Reorgs deeper than this many blocks are not reinjected (geth `legacypool.go:1747`).
pub const MAX_REORG_DEPTH: u64 = 64;

/// The block delta of a head change, from the common ancestor (geth `reset`, `legacypool.go:1773-1810`).
pub struct Branch {
    /// Blocks between the old head and the new head, `|old - new|` as geth counts it.
    pub depth: u64,
    pub discarded: Vec<Arc<super::PoolTx>>,
    pub included: BTreeSet<B256>,
}

/// What [`Pool::reset_reorg`] did with the discarded transactions.
#[derive(Debug, PartialEq, Eq)]
pub enum Reorged {
    /// One admission result per discarded transaction that the new branch does not include.
    Reinjected(Vec<Result<(), Error>>),
    /// The reorg is deeper than [`MAX_REORG_DEPTH`]: the pool followed the head and nothing was reinjected.
    TooDeep { depth: u64 },
}

impl Pool {
    /// geth `Add(txs, sync)` for remote transactions: basic checks, then each transaction under the
    /// pool lock, then one reorg run for the accounts that may have become promotable
    /// (`legacypool.go:1244-1340`). One result per transaction.
    pub fn add_batch<S: StateRead>(
        &mut self,
        state: &S,
        now: u64,
        txs: &[Arc<super::PoolTx>],
    ) -> Vec<Result<(), Error>> {
        self.now = now;
        let mut res: Vec<Result<(), Error>> = Vec::with_capacity(txs.len());
        let mut any = false;
        for t in txs {
            if self.all.get(&t.hash).is_some() {
                res.push(Err(Error::AlreadyKnown));
                continue;
            }
            match validate::basics(self, t) {
                Ok(()) => {
                    any = true;
                    res.push(Ok(()));
                }
                Err(e) => res.push(Err(e)),
            }
        }
        if !any {
            return res;
        }
        let mut dirty = BTreeSet::new();
        for (i, t) in txs.iter().enumerate() {
            if res[i].is_err() {
                continue;
            }
            match self.add_one(state, t) {
                Ok(replaced) => {
                    if !replaced {
                        dirty.insert(t.from());
                    }
                }
                Err(e) => res[i] = Err(e),
            }
        }
        let accounts: Vec<Address> = dirty.into_iter().collect();
        let promoted = self.promote_executables(state, &accounts);
        self.truncate_pending(state);
        self.truncate_queue();
        self.flush_events(promoted);
        self.changes_since_reorg = 0;
        res
    }

    /// Decode and add: each `raw` is a consensus encoding. A decode failure is that transaction's result.
    pub fn add_raw<S: StateRead>(
        &mut self,
        state: &S,
        now: u64,
        raws: &[&[u8]],
    ) -> Vec<Result<(), Error>> {
        let chain = self.spec.chain_id;
        let mut out: Vec<Result<(), Error>> = Vec::with_capacity(raws.len());
        let mut txs = Vec::new();
        let mut at = Vec::new();
        for (i, r) in raws.iter().enumerate() {
            out.push(Ok(()));
            match super::PoolTx::decode(r, chain) {
                Ok(t) => {
                    txs.push(t);
                    at.push(i);
                }
                Err(e) => out[i] = Err(e),
            }
        }
        for (k, r) in self.add_batch(state, now, &txs).into_iter().enumerate() {
            out[at[k]] = r;
        }
        out
    }

    /// `add`, `legacypool.go:944-1130`. `Ok(true)` means the transaction replaced one or went straight to
    /// pending, so the account is not marked for promotion.
    fn add_one<S: StateRead>(&mut self, state: &S, tx: &Tx) -> Result<bool, Error> {
        if self.all.get(&tx.hash).is_some() {
            return Err(Error::AlreadyKnown);
        }
        validate::with_state(self, state, tx)?;
        let from = tx.from();
        if let Some(r) = self.try_special_fast_path(state, from, tx) {
            return r;
        }
        let global = self.cfg.global_slots.saturating_add(self.cfg.global_queue);
        let slots = self.all.slots() + tx.slots();
        if u64::try_from(slots).unwrap_or(u64::MAX) > global {
            if self.priced.underpriced(&self.all, tx) {
                return Err(Error::Underpriced);
            }
            if self.changes_since_reorg
                > usize::try_from(self.cfg.global_slots / 4).unwrap_or(usize::MAX)
            {
                return Err(Error::PoolOverflow);
            }
            let need = slots - usize::try_from(global).unwrap_or(usize::MAX);
            let Some(drop) = self.priced.discard(&self.all, need) else {
                return Err(Error::PoolOverflow);
            };
            if self.is_gapped(state, from, tx) {
                let replaces_pending = drop.iter().any(|d| {
                    self.pending
                        .get(&d.from())
                        .is_some_and(|l| l.contains(d.tx.nonce))
                });
                if replaces_pending {
                    for d in &drop {
                        self.priced.put(d);
                    }
                    return Err(Error::FutureReplacePending);
                }
            }
            for d in &drop {
                let dropped = self.remove_tx(state, d.hash, false);
                self.changes_since_reorg += dropped;
            }
        }
        // Replace an executable transaction.
        if let Some(list) = self
            .pending
            .get_mut(&from)
            .filter(|l| l.contains(tx.tx.nonce))
        {
            let Add::Inserted(old) = list.add(tx, self.cfg.price_bump) else {
                return Err(Error::ReplaceUnderpriced);
            };
            if let Some(o) = &old {
                self.all.remove(&o.hash);
                self.priced.removed(1, &self.all);
            }
            self.all.add(tx);
            self.priced.put(tx);
            self.queue_tx_event(tx);
            self.queue.bump(from, self.now);
            return Ok(old.is_some());
        }
        self.enqueue_tx(state, tx, true)
    }

    /// `isGapped`, `legacypool.go:1132-1156`: whether the transaction cannot execute even after the
    /// queued ones of its account are promoted.
    fn is_gapped<S: StateRead>(&mut self, state: &S, from: Address, tx: &Tx) -> bool {
        let next = self.nonces.get(state, from);
        if tx.tx.nonce <= next {
            return false;
        }
        let Some(q) = self.queue.get(&from) else {
            return true;
        };
        (next..tx.tx.nonce).any(|n| !q.contains(n))
    }

    /// `promoteTx`, `legacypool.go:1182-1215`.
    pub(crate) fn promote_tx(&mut self, addr: Address, tx: &Tx) -> bool {
        let list = self.pending.entry(addr).or_insert_with(|| List::new(true));
        let Add::Inserted(old) = list.add(tx, self.cfg.price_bump) else {
            self.all.remove(&tx.hash);
            self.priced.removed(1, &self.all);
            return false;
        };
        if let Some(o) = &old {
            self.all.remove(&o.hash);
            self.priced.removed(1, &self.all);
        }
        self.nonces.set(addr, tx.tx.nonce.saturating_add(1));
        self.queue.bump(addr, self.now);
        true
    }

    /// `removeTx`, `legacypool.go:1386-1440`: drop a transaction; what a strict pending list loses by it goes
    /// back to the queue. Returns how many pending transactions were removed or demoted.
    pub(crate) fn remove_tx<S: StateRead>(
        &mut self,
        state: &S,
        hash: B256,
        out_of_band: bool,
    ) -> usize {
        let Some(tx) = self.all.get(&hash).cloned() else {
            return 0;
        };
        let addr = tx.from();
        self.all.remove(&hash);
        if out_of_band {
            self.priced.removed(1, &self.all);
        }
        if let Some(p) = self.pending.get_mut(&addr) {
            let (removed, invalids) = p.remove(&tx);
            if removed {
                if p.is_empty() {
                    self.pending.remove(&addr);
                }
                for t in &invalids {
                    // The error is geth's to ignore too: the transaction stays in the lookup.
                    let _ = self.enqueue_tx(state, t, false);
                }
                self.nonces.set_if_lower(state, addr, tx.tx.nonce);
                return 1 + invalids.len();
            }
        }
        self.queue.remove(addr, &tx);
        0
    }

    /// `reset` plus `runReorg` for a new head, `legacypool.go:1543-1733`, `:1736-1830` (reinjection: [`Pool::reset_reorg`]). `state` is the state of the new head.
    pub fn reset<S: StateRead>(
        &mut self,
        state: &S,
        now: u64,
        number: u64,
        gas_limit: u64,
    ) -> Result<(), Error> {
        self.now = now;
        self.head = super::Head::new(&self.spec, number, gas_limit)?;
        self.nonces = Default::default();
        self.forward_events(state);
        let all = self.queue.addresses();
        let promoted = self.promote_executables(state, &all);
        self.demote_unexecutables(state);
        if self.spec.london.is_active(number.saturating_add(1)) {
            let base = tier_price(&self.spec, number.saturating_add(1));
            self.priced.set_base_fee(base, &self.all);
        } else {
            self.priced.reheap(&self.all);
        }
        let next: BTreeMap<Address, u64> = self
            .pending
            .iter()
            .filter_map(|(a, l)| l.last().map(|t| (*a, t.tx.nonce.saturating_add(1))))
            .collect();
        self.nonces.set_all(next);
        self.truncate_pending(state);
        self.truncate_queue();
        self.flush_events(promoted);
        self.changes_since_reorg = 0;
        Ok(())
    }

    /// [`Pool::reset`] for a head that is not a child of the old head. `branch.discarded` are the transactions of
    /// the blocks the old branch lost, `branch.included` the hashes of those of the new branch (geth `reset`,
    /// `legacypool.go:1736-1816`: `TxDifference(discarded, included)`); those not included come back through
    /// normal admission on the new head state (`:1868-1871`, `addTxsLocked`), so nonce, balance, fee and
    /// bounds are checked again and a repeated call refuses them as already known. Deeper than
    /// [`MAX_REORG_DEPTH`] blocks (`:1747`, `depth > 64`) the pool is reset without reinjection.
    pub fn reset_reorg<S: StateRead>(
        &mut self,
        state: &S,
        now: u64,
        number: u64,
        gas_limit: u64,
        branch: &Branch,
    ) -> Result<Reorged, Error> {
        self.reset(state, now, number, gas_limit)?;
        if branch.depth > MAX_REORG_DEPTH {
            return Ok(Reorged::TooDeep {
                depth: branch.depth,
            });
        }
        let lost: Vec<_> = branch
            .discarded
            .iter()
            .filter(|t| !branch.included.contains(&t.hash))
            .cloned()
            .collect();
        Ok(Reorged::Reinjected(self.add_batch(state, now, &lost)))
    }

    /// `demoteUnexecutables`, `legacypool.go:2043-2109` (the full walk).
    fn demote_unexecutables<S: StateRead>(&mut self, state: &S) {
        let addrs: Vec<Address> = self.pending.keys().copied().collect();
        let gas_limit = self.head.gas_limit;
        for a in addrs {
            let acct = state.account(a);
            let (nonce, bal) = acct
                .as_ref()
                .map_or((0, Default::default()), |x| (x.nonce, x.balance.low()));
            let Some(list) = self.pending.get_mut(&a) else {
                continue;
            };
            let olds = list.forward(nonce);
            let (drops, invalids) = list.filter(bal, gas_limit);
            let gapped = if !list.is_empty() && !list.contains(nonce) {
                list.cap(0)
            } else {
                Vec::new()
            };
            let empty = list.is_empty();
            for t in olds.iter().chain(drops.iter()) {
                self.all.remove(&t.hash);
            }
            for t in invalids.iter().chain(gapped.iter()) {
                let _ = self.enqueue_tx(state, t, false);
            }
            self.priced.removed(olds.len() + drops.len(), &self.all);
            if empty {
                self.pending.remove(&a);
            }
        }
    }

    /// `truncatePending`, `legacypool.go:1921-1999`: above `global_slots`, the accounts with more than
    /// `account_slots` transactions lose their highest nonces, the largest first, until they are level.
    /// Ties between accounts of equal length go by address (geth: by chance).
    pub(crate) fn truncate_pending<S: StateRead>(&mut self, state: &S) {
        let global = usize::try_from(self.cfg.global_slots).unwrap_or(usize::MAX);
        let acct_slots = usize::try_from(self.cfg.account_slots).unwrap_or(usize::MAX);
        let mut pending = 0usize;
        let mut spammers: Vec<(usize, Address)> = Vec::new();
        for (a, l) in &self.pending {
            pending += l.len();
            if l.len() > acct_slots {
                spammers.push((l.len(), *a));
            }
        }
        if pending <= global {
            return;
        }
        spammers.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        let mut spammers = spammers.into_iter();
        let mut offenders: Vec<Address> = Vec::new();
        while pending > global {
            let Some((_, offender)) = spammers.next() else {
                break;
            };
            offenders.push(offender);
            if offenders.len() > 1 {
                let threshold = self.pending.get(&offender).map_or(0, List::len);
                while pending > global
                    && self
                        .pending
                        .get(&offenders[offenders.len() - 2])
                        .map_or(0, List::len)
                        > threshold
                {
                    for a in offenders[..offenders.len() - 1].iter().copied() {
                        self.cap_one(state, a);
                        pending = pending.saturating_sub(1);
                    }
                }
            }
        }
        if pending > global && !offenders.is_empty() {
            while pending > global
                && self
                    .pending
                    .get(&offenders[offenders.len() - 1])
                    .map_or(0, List::len)
                    > acct_slots
            {
                for a in offenders.clone() {
                    self.cap_one(state, a);
                    pending = pending.saturating_sub(1);
                }
            }
        }
    }

    /// `list.Cap(len - 1)` on one account, with the lookup and nonce bookkeeping.
    fn cap_one<S: StateRead>(&mut self, state: &S, a: Address) {
        let Some(l) = self.pending.get_mut(&a) else {
            return;
        };
        let caps = l.cap(l.len().saturating_sub(1));
        for t in &caps {
            self.all.remove(&t.hash);
            self.nonces.set_if_lower(state, a, t.tx.nonce);
        }
        self.priced.removed(caps.len(), &self.all);
    }

    /// geth `Pending`, `legacypool.go:524-583`: the executable transactions by sender, for the block
    /// packer. With `min_tip`, a list is cut at the first transaction whose effective tip at `base_fee`
    /// is below it, except special transactions, which are never cut for price; with `gas_cap`, at the
    /// first transaction above it.
    pub fn pending(
        &self,
        min_tip: Option<u128>,
        base_fee: u128,
        gas_cap: Option<u64>,
    ) -> BTreeMap<Address, Vec<Arc<super::PoolTx>>> {
        let mut out = BTreeMap::new();
        for (a, l) in &self.pending {
            let mut txs = l.flatten();
            for (i, t) in txs.iter().enumerate() {
                let low = min_tip.is_some_and(|m| {
                    !t.is_special()
                        && t.fee_cap()
                            .checked_sub(base_fee)
                            .is_none_or(|room| room.min(t.tip_cap()) < m)
                });
                if low || gas_cap.is_some_and(|c| t.tx.gas > c) {
                    txs.truncate(i);
                    break;
                }
            }
            if !txs.is_empty() {
                out.insert(*a, txs);
            }
        }
        out
    }
}
