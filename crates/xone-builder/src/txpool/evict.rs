//! Slot accounting, the price heaps that choose what to evict, and queue expiry.
//!
//! geth `lookup` (`legacypool.go:2184-2316`) and `pricedList` (`list.go:509-718`). The two heaps are
//! `container/heap` binary heaps; their length, stale entries included, decides which heap `Discard`
//! pops from (`urgentRatio : floatingRatio` = 4 : 1), so the array operations are the same as Go's.

use super::{list::Tx, Pool};
use alloy_primitives::B256;
use std::{cmp::Ordering, collections::BTreeMap};
use xone_state::StateRead;

/// `lookup`: every pooled transaction by hash, and the slots they use.
#[derive(Default)]
pub(crate) struct Lookup {
    txs: BTreeMap<B256, Tx>,
    slots: usize,
}

impl Lookup {
    pub(crate) fn get(&self, h: &B256) -> Option<&Tx> {
        self.txs.get(h)
    }
    pub(crate) fn add(&mut self, t: &Tx) {
        self.slots += t.slots();
        self.txs.insert(t.hash, t.clone());
    }
    pub(crate) fn remove(&mut self, h: &B256) {
        if let Some(t) = self.txs.remove(h) {
            self.slots = self.slots.saturating_sub(t.slots());
        }
    }
    pub(crate) fn count(&self) -> usize {
        self.txs.len()
    }
    pub(crate) fn slots(&self) -> usize {
        self.slots
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &Tx> {
        self.txs.values()
    }
}

/// Order key of an effective tip `min(tip, feeCap - baseFee)`, which may be negative
/// (`EffectiveGasTipValue`, `core/types/transaction.go:424-438`).
fn eff_tip(t: &Tx, base: u128) -> (bool, u128) {
    match t.fee_cap().checked_sub(base) {
        Some(room) => (true, room.min(t.tip_cap())),
        None => (false, u128::MAX - (base - t.fee_cap())),
    }
}

#[derive(Default)]
struct PriceHeap {
    /// geth: set on the urgent heap only.
    base_fee: Option<u128>,
    list: Vec<Tx>,
}

impl PriceHeap {
    /// `priceHeap.cmp`, `list.go:528-541`.
    fn cmp(&self, a: &Tx, b: &Tx) -> Ordering {
        if let Some(base) = self.base_fee {
            let c = eff_tip(a, base).cmp(&eff_tip(b, base));
            if c != Ordering::Equal {
                return c;
            }
        }
        a.fee_cap()
            .cmp(&b.fee_cap())
            .then_with(|| a.tip_cap().cmp(&b.tip_cap()))
    }

    /// `priceHeap.Less`: price, then the higher nonce is "less".
    fn less(&self, i: usize, j: usize) -> bool {
        match self.cmp(&self.list[i], &self.list[j]) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => self.list[i].tx.nonce > self.list[j].tx.nonce,
        }
    }

    fn up(&mut self, mut j: usize) {
        while j > 0 {
            let i = (j - 1) / 2;
            if !self.less(j, i) {
                break;
            }
            self.list.swap(i, j);
            j = i;
        }
    }

    fn down(&mut self, i0: usize, n: usize) {
        let mut i = i0;
        loop {
            let j1 = 2 * i + 1;
            if j1 >= n {
                break;
            }
            let mut j = j1;
            if j1 + 1 < n && self.less(j1 + 1, j1) {
                j = j1 + 1;
            }
            if !self.less(j, i) {
                break;
            }
            self.list.swap(i, j);
            i = j;
        }
    }

    fn push(&mut self, t: Tx) {
        self.list.push(t);
        self.up(self.list.len() - 1);
    }

    fn pop(&mut self) -> Option<Tx> {
        let n = self.list.len().checked_sub(1)?;
        self.list.swap(0, n);
        self.down(0, n);
        self.list.pop()
    }

    fn init(&mut self) {
        let n = self.list.len();
        for i in (0..n / 2).rev() {
            self.down(i, n);
        }
    }
}

/// `pricedList`.
#[derive(Default)]
pub(crate) struct Priced {
    stales: i64,
    urgent: PriceHeap,
    floating: PriceHeap,
}

const URGENT_RATIO: usize = 4;
const FLOATING_RATIO: usize = 1;

impl Priced {
    pub(crate) fn put(&mut self, t: &Tx) {
        self.urgent.push(t.clone());
    }

    /// `Removed`: count the stale entries and rebuild when they pass a quarter of the heaps.
    pub(crate) fn removed(&mut self, count: usize, all: &Lookup) {
        self.stales += i64::try_from(count).unwrap_or(i64::MAX);
        let total = self.urgent.list.len() + self.floating.list.len();
        if usize::try_from(self.stales).unwrap_or(usize::MAX) <= total / 4 {
            return;
        }
        self.reheap(all);
    }

    fn underpriced_for(stales: &mut i64, h: &mut PriceHeap, all: &Lookup, tx: &Tx) -> bool {
        while let Some(head) = h.list.first() {
            if all.get(&head.hash).is_none() {
                *stales -= 1;
                h.pop();
                continue;
            }
            break;
        }
        match h.list.first() {
            None => false,
            Some(head) => h.cmp(head, tx) != Ordering::Less,
        }
    }

    /// `Underpriced`: worse than, or as cheap as, the cheapest tracked transaction in every non-empty heap.
    pub(crate) fn underpriced(&mut self, all: &Lookup, tx: &Tx) -> bool {
        let s = &mut self.stales;
        (Self::underpriced_for(s, &mut self.urgent, all, tx) || self.urgent.list.is_empty())
            && (Self::underpriced_for(s, &mut self.floating, all, tx)
                || self.floating.list.is_empty())
            && (!self.urgent.list.is_empty() || !self.floating.list.is_empty())
    }

    /// `Discard`: pick the cheapest transactions that free `slots` slots, or none.
    pub(crate) fn discard(&mut self, all: &Lookup, slots: usize) -> Option<Vec<Tx>> {
        let mut slots = i64::try_from(slots).unwrap_or(i64::MAX);
        let mut drop: Vec<Tx> = Vec::new();
        while slots > 0 {
            if self.urgent.list.len() * FLOATING_RATIO > self.floating.list.len() * URGENT_RATIO {
                let Some(tx) = self.urgent.pop() else { break };
                if all.get(&tx.hash).is_none() {
                    self.stales -= 1;
                    continue;
                }
                self.floating.push(tx);
            } else {
                let Some(tx) = self.floating.pop() else { break };
                if all.get(&tx.hash).is_none() {
                    self.stales -= 1;
                    continue;
                }
                slots -= i64::try_from(tx.slots()).unwrap_or(i64::MAX);
                drop.push(tx);
            }
        }
        if slots > 0 {
            for t in drop {
                self.urgent.push(t);
            }
            return None;
        }
        Some(drop)
    }

    /// `Reheap`: rebuild from the live transactions and move the cheaper fifth to the floating heap.
    pub(crate) fn reheap(&mut self, all: &Lookup) {
        self.stales = 0;
        self.urgent.list = all.values().cloned().collect();
        self.urgent.init();
        let n = self.urgent.list.len() * FLOATING_RATIO / (URGENT_RATIO + FLOATING_RATIO);
        self.floating.list = (0..n).filter_map(|_| self.urgent.pop()).collect();
        self.floating.init();
    }

    /// `SetBaseFee`.
    pub(crate) fn set_base_fee(&mut self, base: u128, all: &Lookup) {
        self.urgent.base_fee = Some(base);
        self.reheap(all);
    }
}

impl Pool {
    /// The body of the eviction tick (`legacypool.go:405-410`): every transaction of an account that
    /// has been idle in the queue for longer than `lifetime_secs`. Pending transactions never expire:
    /// they leave when a block includes them, when the account can no longer pay, or on truncation.
    pub fn evict_expired<S: StateRead>(&mut self, state: &S, now: u64) -> usize {
        self.now = now;
        let hashes = self.queue.evict_list(now, self.cfg.lifetime_secs);
        for h in &hashes {
            self.remove_tx(state, *h, true);
        }
        hashes.len()
    }
}
