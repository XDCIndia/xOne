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
    fn status(&self) -> (usize, usize) {
        self.lock().pool.len()
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

impl Service for PoolService {
    fn name(&self) -> &str {
        "pool"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let mut head = self.shared.core.head.subscribe();
        let shared = self.shared.clone();
        self.task = Some(self.rt.spawn(async move {
            while head.changed().await.is_ok() {
                shared.reset();
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
