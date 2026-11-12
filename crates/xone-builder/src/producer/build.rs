//! Block construction from the pool's pending set (`spec/producer/construction.md`).
//!
//! [`select`] is geth `fillTransactions` and `commitTransactions` (`miner/worker.go:557-723`, `:725-806`)
//! with the XDC rules: the leading special transactions of every account go first and outside the price
//! check (`miner/worker.go:762-773`, `splitSpecialTxs` `:808-834`); the rest goes by price and nonce
//! (`core/txpool/txorder/ordering.go`); a transaction that does not fit the gas left skips its account; a
//! sign transaction with short calldata or for a block not below the one being built is skipped, and at
//! most `2 * maxMasternodesV2` go in one block (`:661-690`). An executed transaction is reported by the
//! caller's `commit`, which is geth's `commitTransaction`.
//!
//! [`assemble`] is `Finalize` and `FinalizeAndAssemble`: the roots that execution produced go into the
//! header (`xdpos.go:1713-1717`, `engine_v2/engine.go:798-801`).

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use alloy_primitives::{Address, Bloom, B256};
use xone_primitives::Header;

use crate::txpool::{special::SIGN_CALLDATA_LEN, PoolTx};

/// geth `params.TxGas`: the least a transaction may declare.
const TX_GAS: u64 = 21_000;
/// `Keccak256(RLP([]))`, geth `types.EmptyUncleHash` (`core/types/hashes.go:29`).
pub const EMPTY_UNCLE_HASH: B256 =
    alloy_primitives::b256!("1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347");

/// What the caller's execution says about one transaction (geth `commitTransaction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// Applied; this much gas was used.
    Used(u64),
    /// `ErrNonceTooLow`: the pool is behind the head. The next one of the sender is tried.
    NonceTooLow,
    /// Anything else: the sender's later transactions are dropped (nonce too high).
    Invalid,
}

/// A pending transaction and when the pool first saw it (geth `LazyTransaction.Time`, the tie break).
#[derive(Debug, Clone)]
pub struct Cand {
    pub tx: Arc<PoolTx>,
    pub seen: u64,
}

pub struct Params {
    /// Number of the block being built.
    pub number: u64,
    pub gas_limit: u64,
    /// `--miner.blockvotegascap`: stop once this much gas is used. Zero or the gas limit: no cap.
    pub soft_gas_cap: u64,
    pub base_fee: Option<u128>,
    /// `2 * maxMasternodesV2` (`miner/worker.go:123-128`).
    pub max_sign_txs: usize,
}

/// Heads of the accounts' queues, best first: effective tip, then earliest seen, then lowest address.
struct Heap {
    rest: BTreeMap<Address, VecDeque<Cand>>,
    heads: Vec<(Address, Cand, u128)>,
}

fn tip(c: &Cand, base_fee: Option<u128>) -> Option<u128> {
    let Some(b) = base_fee else {
        return Some(c.tx.tip_cap());
    };
    match c.tx.fee_cap().checked_sub(b) {
        // a special transaction at price 0 yields no tip (`ordering.go:42-52`); any other is refused
        None => c.tx.is_special().then_some(0),
        Some(room) => Some(room.min(c.tx.tip_cap())),
    }
}

impl Heap {
    fn new(pending: BTreeMap<Address, Vec<Cand>>, base_fee: Option<u128>) -> Self {
        let mut h = Self {
            rest: BTreeMap::new(),
            heads: Vec::new(),
        };
        for (a, txs) in pending {
            let mut q: VecDeque<Cand> = txs.into();
            if let Some(first) = q.pop_front() {
                if let Some(t) = tip(&first, base_fee) {
                    h.heads.push((a, first, t));
                    h.rest.insert(a, q);
                }
            }
        }
        h
    }

    // ponytail: a linear scan per pick; a binary heap when pending sets reach tens of thousands.
    fn best(&self) -> Option<usize> {
        (0..self.heads.len()).min_by(|&i, &j| {
            let (a, x, tx) = &self.heads[i];
            let (b, y, ty) = &self.heads[j];
            ty.cmp(tx).then(x.seen.cmp(&y.seen)).then(a.cmp(b))
        })
    }

    fn pop(&mut self, i: usize) {
        let (a, _, _) = self.heads.swap_remove(i);
        self.rest.remove(&a);
    }

    /// `Shift`: the sender's next transaction takes the place, when it is admissible.
    fn shift(&mut self, i: usize, base_fee: Option<u128>) {
        let a = self.heads[i].0;
        if let Some(next) = self.rest.get_mut(&a).and_then(VecDeque::pop_front) {
            if let Some(t) = tip(&next, base_fee) {
                self.heads[i] = (a, next, t);
                return;
            }
        }
        self.pop(i);
    }
}

/// The special transactions at the front of each account's queue, and the rest (`splitSpecialTxs`): only the
/// uninterrupted special run at the front moves; a special transaction behind a paid nonce stays.
fn split_special(
    pending: BTreeMap<Address, Vec<Cand>>,
) -> (BTreeMap<Address, Vec<Cand>>, BTreeMap<Address, Vec<Cand>>) {
    let (mut special, mut normal) = (BTreeMap::new(), BTreeMap::new());
    for (a, txs) in pending {
        let (s, n): (Vec<Cand>, Vec<Cand>) = {
            let k = txs.iter().take_while(|c| c.tx.is_special()).count();
            let mut v = txs;
            let n = v.split_off(k);
            (v, n)
        };
        if !s.is_empty() {
            special.insert(a, s);
        }
        if !n.is_empty() {
            normal.insert(a, n);
        }
    }
    (special, normal)
}

struct Fill<'a, F> {
    p: &'a Params,
    commit: F,
    out: Vec<Arc<PoolTx>>,
    gas_left: u64,
    used: u64,
    signs: usize,
}

impl<F: FnMut(&PoolTx) -> Commit> Fill<'_, F> {
    /// geth `commitTransactions`.
    fn run(&mut self, pending: BTreeMap<Address, Vec<Cand>>, fee: Option<u128>) {
        let mut heap = Heap::new(pending, fee);
        loop {
            if self.gas_left < TX_GAS {
                return;
            }
            if self.p.soft_gas_cap > 0 && self.used >= self.p.soft_gas_cap {
                return;
            }
            let Some(i) = heap.best() else { return };
            let t = heap.heads[i].1.tx.clone();
            if self.gas_left < t.tx.gas {
                heap.pop(i);
                continue;
            }
            if t.tx.to == Some(xone_evm::route::BLOCK_SIGNERS) {
                let d = t.tx.data.as_ref();
                let signed = d
                    .get(4..36)
                    .map(|w| w[24..].iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)));
                if d.len() < SIGN_CALLDATA_LEN
                    || signed.is_none_or(|n| n >= self.p.number)
                    || self.signs >= self.p.max_sign_txs
                {
                    heap.pop(i);
                    continue;
                }
            }
            match (self.commit)(&t) {
                Commit::Used(g) => {
                    self.out.push(t.clone());
                    // geth refunds what a transaction did not use to the pool
                    self.gas_left = self.gas_left.saturating_sub(g);
                    self.used += g;
                    self.signs += usize::from(t.tx.to == Some(xone_evm::route::BLOCK_SIGNERS));
                    heap.shift(i, fee);
                }
                Commit::NonceTooLow => heap.shift(i, fee),
                Commit::Invalid => heap.pop(i),
            }
        }
    }
}

/// The transactions of a block in order. `commit` executes one on the block's state and says what happened;
/// geth runs the special ones with no base fee check, then the locals and the rest by price.
pub fn select<F: FnMut(&PoolTx) -> Commit>(
    pending: BTreeMap<Address, Vec<Cand>>,
    p: &Params,
    commit: F,
) -> Vec<Arc<PoolTx>> {
    let (special, normal) = split_special(pending);
    let mut f = Fill {
        p,
        commit,
        out: Vec::new(),
        gas_left: p.gas_limit,
        used: 0,
        signs: 0,
    };
    f.run(special, None);
    f.run(normal, p.base_fee);
    f.out
}

/// What execution produced for the header (`BlockOutcome` of `xone-executor`, without its state diff).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub state_root: B256,
    pub tx_root: B256,
    pub receipt_root: B256,
    pub bloom: Bloom,
    pub gas_used: u64,
    /// From Amsterdam: the hash of the block access list the execution built.
    pub bal_hash: Option<B256>,
}

/// geth `types.EmptyWithdrawalsHash` (`core/types/hashes.go:41`).
const EMPTY_WITHDRAWALS_HASH: B256 =
    alloy_primitives::b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");
/// geth `types.EmptyRequestsHash` (`core/types/hashes.go:44`).
const EMPTY_REQUESTS_HASH: B256 =
    alloy_primitives::b256!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

impl From<&xone_executor::BlockOutcome> for Roots {
    fn from(o: &xone_executor::BlockOutcome) -> Self {
        Self {
            state_root: o.state_root,
            tx_root: o.tx_root,
            receipt_root: o.receipt_root,
            bloom: o.bloom,
            gas_used: o.gas_used,
            bal_hash: o.bal_hash,
        }
    }
}

/// `Finalize`: the roots, and the empty uncle hash. From Amsterdam (the execution built a block access list)
/// also geth `core.FillAmsterdamHeader` (`core/state_processor.go:789-809`, called by the worker after
/// `Finalize`, `miner/xdpos_worker.go:693-702` and `:906-915`): the block access list hash and the canonical
/// empty values of the fields before it, which `verifyNoUnsealedFields` requires (`consensus/XDPoS/xdpos.go:770-797`).
/// The body of such a block carries an empty withdrawals list (`types.Withdrawals{}`).
pub fn assemble(mut h: Header, r: &Roots) -> Header {
    h.state_root = r.state_root;
    h.tx_root = r.tx_root;
    h.receipt_root = r.receipt_root;
    h.bloom = r.bloom;
    h.gas_used = r.gas_used;
    h.uncle_hash = EMPTY_UNCLE_HASH;
    if let Some(bal) = r.bal_hash {
        h.withdrawals_hash = Some(EMPTY_WITHDRAWALS_HASH);
        h.blob_gas_used = Some(0);
        h.excess_blob_gas = Some(0);
        h.parent_beacon_root = Some(B256::ZERO);
        h.requests_hash = Some(EMPTY_REQUESTS_HASH);
        h.block_access_list_hash = Some(bal);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(bal_hash: Option<B256>) -> Roots {
        Roots {
            state_root: B256::with_last_byte(1),
            tx_root: B256::with_last_byte(2),
            receipt_root: B256::with_last_byte(3),
            bloom: Bloom::ZERO,
            gas_used: 0,
            bal_hash,
        }
    }

    /// An Amsterdam block carries the access list hash and geth's canonical empty values before it
    /// (`core.FillAmsterdamHeader`); without them every node refused the block (`verifyNoUnsealedFields`, gate D
    /// devnet from block 58). A block before Amsterdam keeps all of them unset.
    #[test]
    fn amsterdam_fields_follow_fill_amsterdam_header() {
        let bal = B256::with_last_byte(9);
        let h = assemble(Header::default(), &roots(Some(bal)));
        assert_eq!(h.block_access_list_hash, Some(bal));
        assert_eq!(h.withdrawals_hash, Some(EMPTY_WITHDRAWALS_HASH));
        assert_eq!((h.blob_gas_used, h.excess_blob_gas), (Some(0), Some(0)));
        assert_eq!(h.parent_beacon_root, Some(B256::ZERO));
        assert_eq!(h.requests_hash, Some(EMPTY_REQUESTS_HASH));
        assert_eq!(h.slot_number, None);
        let old = assemble(Header::default(), &roots(None));
        assert!(old.withdrawals_hash.is_none() && old.block_access_list_hash.is_none());
        assert!(old.requests_hash.is_none() && old.parent_beacon_root.is_none());
    }
}
