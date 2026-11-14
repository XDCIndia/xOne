//! The `pool` service: `xone_builder::txpool::Pool` fed from gossip and `eth_sendRawTransaction`,
//! reset on every new head of the chain, serving the RPC `PoolApi`.
//!
//! ponytail: the pool is not announced to peers (no `txpool::propagate` wiring) and holds no special
//! transaction authority (`Authority::Syncing`: this node does not mine).

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, PoisonError},
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use tokio::{runtime::Handle, task::JoinHandle};
use xone_builder::{
    producer::build::Cand,
    txpool::{special::Authority, validate::min_gas_price, Config, Pool, PoolTx},
};
use xone_net::eth::Raw;
use xone_rpc::api::PoolApi;
use xone_sync::chain::ChainRead;

use super::{
    core::{now, Core, Dyn},
    from_wire,
};
use crate::lifecycle::{Service, ServiceError};

struct Inner {
    pool: Pool,
    /// Consensus encodings of what the pool holds: the pool keeps decoded transactions only.
    raws: HashMap<B256, Bytes>,
}

pub struct PoolShared {
    inner: Mutex<Inner>,
    core: Arc<Core>,
}

impl PoolShared {
    pub fn new(core: Arc<Core>) -> Result<Self, String> {
        let (number, _) = core.head();
        let gas = core.hist.read().tip().header.gas_limit;
        let pool = Pool::new(
            core.spec.clone(),
            &Config::default(),
            Authority::Syncing,
            number,
            gas,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            inner: Mutex::new(Inner {
                pool,
                raws: HashMap::new(),
            }),
            core,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` on the state of the chain head.
    fn on_head<R>(&self, f: impl FnOnce(&Dyn<'_>) -> R) -> Result<R, String> {
        let (_, hash) = self.core.head();
        let (mut f, mut out) = (Some(f), None);
        self.core
            .chain
            .reader()
            .with_state(hash, &mut |s| {
                if let Some(f) = f.take() {
                    out = Some(f(&Dyn(s)));
                }
            })
            .map_err(|e| e.to_string())?;
        out.ok_or_else(|| "no head state".to_owned())
    }

    /// Consensus encodings in, one result per transaction (geth's error text on a refusal).
    /// Executable transactions in the pool (the stats board's pending count).
    pub fn pending_count(&self) -> usize {
        self.lock().pool.stats().0
    }

    pub fn add_raws(&self, raws: &[&[u8]]) -> Vec<Result<B256, String>> {
        let mut g = self.lock();
        let res = match self.on_head(|st| g.pool.add_raw(st, now(), raws)) {
            Ok(r) => r,
            Err(e) => return raws.iter().map(|_| Err(e.clone())).collect(),
        };
        raws.iter()
            .zip(res)
            .map(|(raw, r)| match r {
                Ok(()) => {
                    let h = keccak256(raw);
                    g.raws.insert(h, Bytes::copy_from_slice(raw));
                    Ok(h)
                }
                Err(e) => Err(e.to_string()),
            })
            .collect()
    }

    /// Transactions of a `Transactions` message; a refusal is the sender's business, not ours.
    pub fn add_wire(&self, txs: &[Raw]) {
        let raws: Vec<Bytes> = txs.iter().map(from_wire).collect();
        let refs: Vec<&[u8]> = raws.iter().map(|r| &r[..]).collect();
        let _ = self.add_raws(&refs);
    }

    /// Who may send special transactions (the producer's own sign transactions need its node to be a candidate).
    pub fn set_authority(&self, a: Authority) {
        self.lock().pool.set_authority(a);
    }

    /// The executable transactions for the block packer, with their consensus encodings. The time a transaction
    /// was first seen is not kept by the pool, so ties of price go by sender address.
    pub fn pending_raws(
        &self,
        base_fee: u128,
    ) -> (BTreeMap<Address, Vec<Cand>>, HashMap<B256, Bytes>) {
        let g = self.lock();
        let p = g
            .pool
            .pending(None, base_fee, None)
            .into_iter()
            .map(|(a, v)| (a, v.into_iter().map(|tx| Cand { tx, seen: 0 }).collect()))
            .collect();
        (p, g.raws.clone())
    }

    /// The pool follows the head: promote, demote and drop what the new state made stale.
    pub fn reset(&self) {
        let (number, _) = self.core.head();
        let gas = self.core.hist.read().tip().header.gas_limit;
        let mut g = self.lock();
        let Inner { pool, raws } = &mut *g;
        // a pool that cannot reset keeps its transactions; the next head tries again
        let _ = self.on_head(|st| pool.reset(st, now(), number, gas));
        raws.retain(|h, _| pool.has(h));
    }
}

impl PoolApi for PoolShared {
    /// `(pending, queued)` transaction counts, as geth `txpool_status`. `Pool::len` is
    /// `(transactions, slots)` and is capacity accounting, not this contract.
    fn status(&self) -> (usize, usize) {
        self.lock().pool.stats()
    }

    #[allow(clippy::type_complexity)]
    fn content(&self) -> (BTreeMap<Address, Vec<Bytes>>, BTreeMap<Address, Vec<Bytes>>) {
        let g = self.lock();
        let (p, q) = g.pool.content();
        let conv = |m: BTreeMap<Address, Vec<Arc<PoolTx>>>| {
            m.into_iter()
                .map(|(a, v)| {
                    (
                        a,
                        v.iter()
                            .filter_map(|t| g.raws.get(&t.hash).cloned())
                            .collect(),
                    )
                })
                .collect()
        };
        (conv(p), conv(q))
    }

    fn add_raw(&self, raw: &[u8]) -> Result<B256, String> {
        self.add_raws(&[raw]).remove(0)
    }

    fn pending_nonce(&self, who: Address) -> Option<u64> {
        let mut g = self.lock();
        self.on_head(|st| g.pool.nonce(st, who)).ok()
    }

    fn gas_price(&self) -> U256 {
        U256::from(min_gas_price(&self.core.spec, self.core.head().0))
    }
}

pub struct PoolService {
    shared: Arc<PoolShared>,
    rt: Handle,
    task: Option<JoinHandle<()>>,
}

impl PoolService {
    pub fn new(shared: Arc<PoolShared>, rt: Handle) -> Self {
        Self {
            shared,
            rt,
            task: None,
        }
    }
}

/// The deepest reorg whose discarded transactions go back to the pool (issue #63). A deeper one, or
/// one whose old branch the pool did not see, only resets the pool: a typed outcome, never a walk
/// without end.
pub const MAX_REORG_DEPTH: u64 = 64;

/// A canonical block as the pool saw it: number, parent and the consensus encodings of its
/// transactions.
#[derive(Clone, Debug)]
pub struct SeenBlock {
    pub number: u64,
    pub parent: B256,
    pub txs: Vec<Bytes>,
}

/// Why a head change gave no reinjection set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// The fork point is more than [`MAX_REORG_DEPTH`] blocks below one of the heads.
    TooDeep,
    /// A block of either branch is not known (not seen while canonical, or pruned).
    Unknown(B256),
}

/// The transactions of the branch from `old` back to the common ancestor with `new`, oldest block
/// first, minus every transaction the branch to `new` includes (geth `legacypool.go` `reset`:
/// `discarded` minus `included`). An extension of `old` gives nothing.
pub fn discarded(
    old: B256,
    new: B256,
    get: impl Fn(B256) -> Option<SeenBlock>,
) -> Result<Vec<Bytes>, Delta> {
    let (mut o, mut n) = (old, new);
    let mut ob = get(o).ok_or(Delta::Unknown(o))?;
    let mut nb = get(n).ok_or(Delta::Unknown(n))?;
    let (mut gone, mut kept): (Vec<Vec<Bytes>>, std::collections::HashSet<B256>) =
        (Vec::new(), std::collections::HashSet::new());
    let mut steps = 0u64;
    while o != n {
        steps += 1;
        if steps > 2 * MAX_REORG_DEPTH {
            return Err(Delta::TooDeep);
        }
        if ob.number >= nb.number {
            gone.push(std::mem::take(&mut ob.txs));
            o = ob.parent;
            ob = get(o).ok_or(Delta::Unknown(o))?;
        } else {
            kept.extend(nb.txs.iter().map(keccak256));
            n = nb.parent;
            nb = get(n).ok_or(Delta::Unknown(n))?;
        }
    }
    Ok(gone
        .into_iter()
        .rev()
        .flatten()
        .filter(|t| !kept.contains(&keccak256(t)))
        .collect())
}

/// The canonical blocks the pool has seen, for the old branch of a later reorg; bounded.
#[derive(Default)]
struct Seen {
    blocks: HashMap<B256, SeenBlock>,
}

impl Seen {
    const MAX: usize = 4 * MAX_REORG_DEPTH as usize;

    /// Records the canonical blocks from `head` back to a known block, at most the reorg depth.
    fn record(&mut self, core: &Core, head: (u64, B256)) {
        let (mut n, mut h) = head;
        for _ in 0..=MAX_REORG_DEPTH {
            if self.blocks.contains_key(&h) {
                break;
            }
            let Some(parent) = core.hist.read().header_by_hash(h).map(|x| x.parent_hash) else {
                break;
            };
            let Some(body) = core.body(n, h) else { break };
            self.blocks.insert(
                h,
                SeenBlock {
                    number: n,
                    parent,
                    txs: body.transactions,
                },
            );
            if n == 0 {
                break;
            }
            (n, h) = (n - 1, parent);
        }
        let floor = head.0.saturating_sub(2 * MAX_REORG_DEPTH);
        self.blocks.retain(|_, b| b.number >= floor);
        while self.blocks.len() > Self::MAX {
            let Some(k) = self
                .blocks
                .iter()
                .min_by_key(|(_, b)| b.number)
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.blocks.remove(&k);
        }
    }
}

impl PoolShared {
    /// A head change: the transactions of a discarded branch go back to the pool after the reset,
    /// under the new head's rules (nonce, balance, fee, special-transaction authority). Returns how
    /// many were taken.
    fn follow(&self, seen: &mut Seen, last: (u64, B256), head: (u64, B256)) -> usize {
        if last == head {
            return 0;
        }
        // the new branch is canonical now; the old one is in `seen` (or the delta is unknown)
        seen.record(&self.core, head);
        let back = discarded(last.1, head.1, |h| seen.blocks.get(&h).cloned());
        self.reset();
        match back {
            Ok(txs) if !txs.is_empty() => {
                let refs: Vec<&[u8]> = txs.iter().map(|t| &t[..]).collect();
                self.add_raws(&refs).iter().filter(|r| r.is_ok()).count()
            }
            Ok(_) => 0,
            Err(e) => {
                tracing::debug!(target: "pool", "no reinjection for the head change to {}: {e:?}", head.0);
                0
            }
        }
    }
}

impl Service for PoolService {
    fn name(&self) -> &str {
        "pool"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let mut head = self.shared.core.head.subscribe();
        let shared = self.shared.clone();
        self.task = Some(self.rt.spawn(async move {
            let mut seen = Seen::default();
            let mut last = shared.core.head();
            seen.record(&shared.core, last);
            while head.changed().await.is_ok() {
                // the watch coalesces: only the latest head is processed, once
                let now_head = *head.borrow_and_update();
                shared.follow(&mut seen, last, now_head);
                last = now_head;
            }
        }));
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test: a panic is the failure report
mod tests {
    use super::*;

    fn tx(i: u8) -> Bytes {
        Bytes::from(vec![i; 3])
    }

    /// Blocks named by a byte: `(hash, number, parent, txs)`.
    fn chain(v: &[(u8, u64, u8, &[u8])]) -> HashMap<B256, SeenBlock> {
        v.iter()
            .map(|(h, n, p, t)| {
                (
                    B256::repeat_byte(*h),
                    SeenBlock {
                        number: *n,
                        parent: B256::repeat_byte(*p),
                        txs: t.iter().map(|i| tx(*i)).collect(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn discarded_minus_included_oldest_first() {
        // 1 <- 2 <- 3 <- 4 (old head)      1 <- 2 <- 5 <- 6 <- 7 (new head)
        let m = chain(&[
            (1, 1, 0, &[]),
            (2, 2, 1, &[9]),
            (3, 3, 2, &[10, 11]),
            (4, 4, 3, &[12, 13]),
            (5, 3, 2, &[11]),
            (6, 4, 5, &[]),
            (7, 5, 6, &[13, 14]),
        ]);
        let b = |h: u8| B256::repeat_byte(h);
        let got = discarded(b(4), b(7), |h| m.get(&h).cloned()).unwrap();
        // 11 and 13 are in the new branch; 9 is below the fork
        assert_eq!(got, vec![tx(10), tx(12)]);
        // an extension gives nothing; the same head gives nothing
        assert_eq!(
            discarded(b(5), b(7), |h| m.get(&h).cloned()).unwrap(),
            Vec::<Bytes>::new()
        );
        assert_eq!(
            discarded(b(7), b(7), |h| m.get(&h).cloned()).unwrap(),
            Vec::<Bytes>::new()
        );
        // a branch the pool never saw
        assert_eq!(
            discarded(b(8), b(7), |h| m.get(&h).cloned()),
            Err(Delta::Unknown(b(8)))
        );
    }

    #[test]
    fn a_deep_reorg_is_refused_not_walked() {
        let mut m = HashMap::new();
        let (mut a, mut b) = (B256::ZERO, B256::ZERO);
        // two branches from genesis (hash 0), each 2 * MAX_REORG_DEPTH long
        m.insert(
            B256::ZERO,
            SeenBlock {
                number: 0,
                parent: B256::ZERO,
                txs: vec![],
            },
        );
        for n in 1..=2 * MAX_REORG_DEPTH {
            let (ha, hb) = (
                keccak256([&n.to_be_bytes()[..], b"a"].concat()),
                keccak256([&n.to_be_bytes()[..], b"b"].concat()),
            );
            m.insert(
                ha,
                SeenBlock {
                    number: n,
                    parent: a,
                    txs: vec![],
                },
            );
            m.insert(
                hb,
                SeenBlock {
                    number: n,
                    parent: b,
                    txs: vec![],
                },
            );
            (a, b) = (ha, hb);
        }
        assert_eq!(discarded(a, b, |h| m.get(&h).cloned()), Err(Delta::TooDeep));
    }
}
