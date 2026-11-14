//! One account's transactions by nonce: geth `list` and `SortedMap` (`core/txpool/legacypool/list.go`).
//! `strict` (pending) means nonces are contiguous: removing one demotes everything above it.

use super::PoolTx;
use alloy_primitives::U256;
use std::{collections::BTreeMap, sync::Arc};

pub(crate) type Tx = Arc<PoolTx>;

/// What [`List::add`] did (geth returns `(inserted, old)`).
pub(super) enum Add {
    Rejected,
    Inserted(Option<Tx>),
}

pub(crate) struct List {
    strict: bool,
    txs: BTreeMap<u64, Tx>,
    /// Sum of the costs of the stored transactions. geth `totalcost`.
    total: U256,
}

impl List {
    pub(super) fn new(strict: bool) -> Self {
        Self {
            strict,
            txs: BTreeMap::new(),
            total: U256::ZERO,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.txs.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    pub(super) fn get(&self, nonce: u64) -> Option<&Tx> {
        self.txs.get(&nonce)
    }

    pub(super) fn contains(&self, nonce: u64) -> bool {
        self.txs.contains_key(&nonce)
    }

    pub(super) fn total_cost(&self) -> U256 {
        self.total
    }

    pub(super) fn last(&self) -> Option<&Tx> {
        self.txs.last_key_value().map(|(_, t)| t)
    }

    pub(super) fn flatten(&self) -> Vec<Tx> {
        self.txs.values().cloned().collect()
    }

    fn sub(&mut self, txs: &[Tx]) {
        for t in txs {
            // geth panics on underflow (`list.go:496-505`); a saturating subtraction cannot crash the node.
            self.total = self.total.saturating_sub(t.cost().unwrap_or(U256::MAX));
        }
    }

    /// Replace rule, `list.go:303-364`: a special transaction takes the nonce of a regular one at any price,
    /// a regular one never takes a special one's; otherwise both the fee cap and the tip must rise by
    /// `bump` percent, and each must be higher than before.
    pub(super) fn add(&mut self, tx: &Tx, bump: u64) -> Add {
        let old = self.txs.get(&tx.tx.nonce).cloned();
        if let Some(o) = &old {
            if o.is_special() && !tx.is_special() {
                return Add::Rejected;
            }
            if !tx.is_special() || o.is_special() {
                if o.fee_cap() >= tx.fee_cap() || o.tip_cap() >= tx.tip_cap() {
                    return Add::Rejected;
                }
                let pct = U256::from(100u64.saturating_add(bump));
                let hundred = U256::from(100u64);
                let thr_fee = U256::from(o.fee_cap()).saturating_mul(pct) / hundred;
                let thr_tip = U256::from(o.tip_cap()).saturating_mul(pct) / hundred;
                if U256::from(tx.fee_cap()) < thr_fee || U256::from(tx.tip_cap()) < thr_tip {
                    return Add::Rejected;
                }
            }
        }
        let Some(cost) = tx.cost() else {
            return Add::Rejected;
        };
        let Some(total) = self.total.checked_add(cost) else {
            return Add::Rejected;
        };
        self.total = total;
        if let Some(o) = &old {
            self.sub(std::slice::from_ref(o));
        }
        self.txs.insert(tx.tx.nonce, tx.clone());
        Add::Inserted(old)
    }

    /// Insert without the replace rule (`promoteSpecialTx`, `legacypool.go:793-837`). Unlike geth it
    /// keeps `total` right: geth leaves a special transaction's cost out and panics on its removal.
    pub(super) fn put_special(&mut self, tx: &Tx) -> Option<Tx> {
        let old = self.txs.insert(tx.tx.nonce, tx.clone());
        if let Some(o) = &old {
            self.sub(std::slice::from_ref(o));
        }
        self.total = self.total.saturating_add(tx.cost().unwrap_or(U256::MAX));
        old
    }

    /// Remove every transaction with a nonce below `threshold` (`list.go:366-370`).
    pub(super) fn forward(&mut self, threshold: u64) -> Vec<Tx> {
        let keep = self.txs.split_off(&threshold);
        let removed: Vec<Tx> = std::mem::replace(&mut self.txs, keep)
            .into_values()
            .collect();
        self.sub(&removed);
        removed
    }

    /// Remove what the account can no longer pay (cost above `cost_limit`) or the block cannot hold
    /// (gas above `gas_limit`); a strict list also hands back everything above the lowest removed
    /// nonce as `invalids`, which the caller re-queues (`list.go:385-431`).
    ///
    /// A transaction to a registered TRC21 token is paid from the token's fee capacity too: it
    /// stays while `cost_limit + capacity` covers its cost at the TRC21 gas price (`list.go:397-408`).
    pub(super) fn filter(
        &mut self,
        cost_limit: U256,
        gas_limit: u64,
        trc21: &super::Trc21Fees,
    ) -> (Vec<Tx>, Vec<Tx>) {
        let bad = |t: &Tx| {
            if t.tx.gas > gas_limit {
                return true;
            }
            if let Some((cap, cost)) = trc21.cost(t) {
                return cost.is_none_or(|c| cost_limit.saturating_add(cap) < c);
            }
            t.cost().is_none_or(|c| c > cost_limit)
        };
        let removed: Vec<Tx> = self.txs.values().filter(|t| bad(t)).cloned().collect();
        if removed.is_empty() {
            return (Vec::new(), Vec::new());
        }
        for t in &removed {
            self.txs.remove(&t.tx.nonce);
        }
        let mut invalids = Vec::new();
        if self.strict {
            let lowest = removed.iter().map(|t| t.tx.nonce).min().unwrap_or(u64::MAX);
            let above = self.txs.split_off(&lowest.saturating_add(1));
            invalids = above.into_values().collect();
        }
        self.sub(&removed);
        self.sub(&invalids);
        (removed, invalids)
    }

    /// Keep the `threshold` lowest nonces; return the rest, highest first (`list.go:433-438`).
    pub(super) fn cap(&mut self, threshold: usize) -> Vec<Tx> {
        let mut drops = Vec::new();
        while self.txs.len() > threshold {
            if let Some((_, t)) = self.txs.pop_last() {
                drops.push(t);
            }
        }
        self.sub(&drops);
        drops
    }

    /// `list.go:442-460`: remove the entry at the nonce of `tx` (whichever transaction it holds) and
    /// charge `tx`'s cost; a strict list returns what that leaves non-executable.
    pub(super) fn remove(&mut self, tx: &Tx) -> (bool, Vec<Tx>) {
        let nonce = tx.tx.nonce;
        if self.txs.remove(&nonce).is_none() {
            return (false, Vec::new());
        }
        self.sub(std::slice::from_ref(tx));
        if self.strict {
            let above: Vec<Tx> = self
                .txs
                .split_off(&nonce.saturating_add(1))
                .into_values()
                .collect();
            self.sub(&above);
            return (true, above);
        }
        (true, Vec::new())
    }

    /// The run of consecutive nonces that starts at the lowest one, if that is at or below `start`
    /// (`list.go:212-230`). Removed from the list.
    pub(super) fn ready(&mut self, start: u64) -> Vec<Tx> {
        let mut out = Vec::new();
        let Some(mut next) = self.txs.keys().next().copied() else {
            return out;
        };
        if next > start {
            return out;
        }
        while let Some(t) = self.txs.remove(&next) {
            out.push(t);
            next = next.saturating_add(1);
        }
        self.sub(&out);
        out
    }
}
