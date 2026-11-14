//! The transaction pool: geth-XDC `core/txpool/legacypool` (remote transactions only, no journal, no locals)
//! with the XDC rules on top (special transactions, tier floor). Specification: `spec/txpool/`.
//!
//! * [`validate`]: admission checks against the head ([`validate::basics`], [`validate::with_state`]);
//! * [`special`]: who may send a special transaction and the fast path to pending;
//! * [`pending`], [`queued`]: the executable lists and the nonce-gapped queue;
//! * [`evict`]: the slot count, the price heaps, expiry;
//! * [`propagate`]: announce and fetch as pure functions of the pool's events and the peers' messages.
//!
//! The pool owns no state reader and no clock. Every call that reads the chain takes `state`, the state of
//! the head the pool was last [`Pool::reset`] to, and every call that stamps a time takes `now` in seconds.

pub mod evict;
mod list;
pub mod pending;
pub mod propagate;
pub mod queued;
pub mod special;
pub mod validate;

use alloy_primitives::{keccak256, Address, B256, U256};
use evict::{Lookup, Priced};
use list::{List, Tx};
use queued::{Noncer, Queue};
use special::Authority;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use xone_chainspec::ChainSpec;
use xone_evm::{
    basefee::{GAS_2500X_MULTIPLIER, XDC_BASE_FEE},
    gas::IntrinsicTables,
    route::is_special_tx,
    spec::{rules, SpecError},
};
use xone_executor::tx::{decode, Tx as Decoded, TxError};
use xone_state::StateRead;

/// geth `txSlotSize`, `legacypool.go:51`.
pub const TX_SLOT_SIZE: u64 = 32 * 1024;
/// geth `txMaxSize`, `legacypool.go:57`.
pub const TX_MAX_SIZE: u64 = 4 * TX_SLOT_SIZE;

/// geth `legacypool.Config` and `DefaultConfig` (`legacypool.go:150-183`). Locals and the journal do not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Minimum tip to enter the pool (`PriceLimit`; geth `Init(gasTip)` starts from it).
    pub price_limit: u64,
    /// Percent a replacement must add to both fee cap and tip.
    pub price_bump: u64,
    /// Executable slots per account that are never evicted for fairness.
    pub account_slots: u64,
    /// Executable transactions in the whole pool.
    pub global_slots: u64,
    /// Non-executable transactions per account.
    pub account_queue: u64,
    /// Non-executable transactions in the whole pool.
    pub global_queue: u64,
    /// Seconds an account may stay idle in the queue.
    pub lifetime_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            price_limit: 1,
            price_bump: 10,
            account_slots: 16,
            global_slots: 4096 + 1024,
            account_queue: 64,
            global_queue: 1024,
            lifetime_secs: 3 * 60 * 60,
        }
    }
}

impl Config {
    /// geth `Config.sanitize` (`legacypool.go:185-218`): a zero field becomes its default.
    pub fn sanitized(&self) -> Self {
        let d = Self::default();
        let or = |v: u64, dv: u64| if v < 1 { dv } else { v };
        Self {
            price_limit: or(self.price_limit, d.price_limit),
            price_bump: or(self.price_bump, d.price_bump),
            account_slots: or(self.account_slots, d.account_slots),
            global_slots: or(self.global_slots, d.global_slots),
            account_queue: or(self.account_queue, d.account_queue),
            global_queue: or(self.global_queue, d.global_queue),
            lifetime_secs: or(self.lifetime_secs, d.lifetime_secs),
        }
    }
}

/// A decoded transaction with what the pool needs about it. `hash` is the hash of the consensus
/// encoding and `size` its length (geth `Hash()` and `Size()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolTx {
    pub hash: B256,
    pub size: u64,
    pub tx: Decoded,
}

impl PoolTx {
    /// Decode a consensus-encoded transaction, check its signature for `chain_id` and recover the sender.
    /// Trailing bytes after a typed payload are an error (geth `UnmarshalBinary`): they would give one
    /// transaction two hashes.
    pub fn decode(raw: &[u8], chain_id: u64) -> Result<Arc<Self>, Error> {
        if raw.first().is_some_and(|b| *b < 0x80) {
            let mut rest = raw.get(1..).unwrap_or(&[]);
            let h =
                alloy_rlp::Header::decode(&mut rest).map_err(|e| Error::Decode(e.to_string()))?;
            if !h.list || h.payload_length != rest.len() {
                return Err(Error::Decode("typed transaction: trailing bytes".into()));
            }
        }
        let tx = decode(raw, chain_id).map_err(|e| match e {
            TxError::Type(t) => Error::TypeNotSupported(format!("received type {t}")),
            e => Error::InvalidSender(e.to_string()),
        })?;
        Ok(Arc::new(Self {
            hash: keccak256(raw),
            size: u64::try_from(raw.len()).unwrap_or(u64::MAX),
            tx,
        }))
    }

    pub fn from(&self) -> Address {
        self.tx.from
    }

    /// geth `GasFeeCap`; the gas price of a legacy or access-list transaction.
    pub fn fee_cap(&self) -> u128 {
        self.tx.gas_price
    }

    /// geth `GasTipCap`; equal to the gas price for a legacy or access-list transaction.
    pub fn tip_cap(&self) -> u128 {
        self.tx.priority_fee.unwrap_or(self.tx.gas_price)
    }

    /// geth `types.IsSpecialTx`: a target of 0x89 or 0x90 (`xone_evm::route::is_special_tx`).
    pub fn is_special(&self) -> bool {
        is_special_tx(self.tx.to)
    }

    /// geth `Cost`: `gas * gasPrice + value`; `None` above 2^256-1 (which no balance can pay).
    pub fn cost(&self) -> Option<U256> {
        U256::from(self.tx.gas)
            .checked_mul(U256::from(self.tx.gas_price))?
            .checked_add(self.tx.value)
    }

    /// geth `numSlots`, `legacypool.go:2317`.
    pub fn slots(&self) -> usize {
        usize::try_from(self.size.div_ceil(TX_SLOT_SIZE)).unwrap_or(usize::MAX)
    }
}

/// Why a transaction was not admitted. `Display` starts with geth's error text; [`Error::name`] is that
/// text alone (the oracle scenarios compare it).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("already known")]
    AlreadyKnown,
    #[error("transaction decode: {0}")]
    Decode(String),
    #[error("invalid sender: {0}")]
    InvalidSender(String),
    #[error("transaction type not supported: {0}")]
    TypeNotSupported(String),
    #[error("oversized data: transaction size {size}, limit {limit}")]
    Oversized { size: u64, limit: u64 },
    #[error("max initcode size exceeded: code size {size} limit {limit}")]
    MaxInitCodeSize { size: u64, limit: u64 },
    #[error("transaction gas limit too high: cap {cap}, tx {gas}")]
    GasLimitTooHigh { cap: u64, gas: u64 },
    #[error("exceeds block gas limit")]
    GasLimit,
    #[error("max priority fee per gas higher than max fee per gas")]
    TipAboveFeeCap,
    #[error("nonce has max value")]
    NonceMax,
    #[error("special transaction sender is not a signer: {0}")]
    SpecialTxNotFromSigner(Address),
    #[error("intrinsic gas too low: gas {gas}, minimum needed {need}")]
    IntrinsicGas { gas: u64, need: u64 },
    #[error("insufficient gas for floor data gas cost: gas {gas}, minimum needed {need}")]
    FloorDataGas { gas: u64, need: u64 },
    #[error("transaction gas price below minimum: gas tip cap {tip}, minimum needed {min}")]
    GasPriceTooLow { tip: u128, min: u128 },
    #[error("nonce too low: next nonce {next}, tx nonce {tx}")]
    NonceTooLow { next: u64, tx: u64 },
    #[error("insufficient funds for gas * price + value")]
    InsufficientFunds,
    #[error("transaction gas price under xdc tier minimum")]
    UnderMinGasPrice,
    #[error("gapped-nonce tx from delegated accounts")]
    OutOfOrderTxFromDelegated,
    #[error("in-flight transaction limit reached for delegated accounts")]
    InflightTxLimitReached,
    #[error("transaction underpriced")]
    Underpriced,
    #[error("replacement transaction underpriced")]
    ReplaceUnderpriced,
    #[error("txpool is full")]
    PoolOverflow,
    #[error("future transaction tries to replace pending")]
    FutureReplacePending,
    #[error("duplicate a special transaction")]
    DuplicateSpecialTransaction,
    #[error("chain rules at the head: {0}")]
    Spec(String),
    /// geth-XDC `ErrBlacklisted` (`legacypool.go:67`, checked at `:640-655`). The relaying peer is
    /// not penalised for it.
    #[error("transaction sender or recipient is blacklisted: {side} {address}")]
    Denylisted {
        side: &'static str,
        address: Address,
    },
}

impl From<SpecError> for Error {
    fn from(e: SpecError) -> Self {
        Self::Spec(e.to_string())
    }
}

impl Error {
    /// The text of the geth error without its detail (everything before the first `": "`).
    pub fn name(&self) -> String {
        let s = self.to_string();
        s.split_once(": ").map_or(s.clone(), |(a, _)| a.to_owned())
    }
}

/// Where a transaction is (geth `TxStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Queued,
    Pending,
}

/// The head the pool validates against.
pub(crate) struct Head {
    pub(crate) number: u64,
    pub(crate) gas_limit: u64,
    /// The gas tables of geth `Config.Rules(head.Number, ..)` as xone-evm computes them, built once per head.
    pub(crate) gas: IntrinsicTables,
    pub(crate) rules: xone_evm::spec::XdcRules,
}

impl Head {
    pub(crate) fn new(spec: &ChainSpec, number: u64, gas_limit: u64) -> Result<Self, Error> {
        let r = rules(spec, number)?;
        Ok(Self {
            number,
            gas_limit,
            gas: IntrinsicTables::new(&r),
            rules: r,
        })
    }
}

pub struct Pool {
    pub(crate) spec: ChainSpec,
    pub(crate) cfg: Config,
    /// geth `gasTip`: `Init(gasTip)` and `SetGasTip`.
    pub(crate) min_tip: u128,
    pub(crate) head: Head,
    pub(crate) authority: Authority,
    pub(crate) pending: BTreeMap<Address, List>,
    pub(crate) queue: Queue,
    pub(crate) all: Lookup,
    pub(crate) priced: Priced,
    pub(crate) nonces: Noncer,
    /// Drops since the last reorg run (`changesSinceReorg`).
    pub(crate) changes_since_reorg: usize,
    /// The time of the entry point being served: queue heartbeats are stamped with it.
    pub(crate) now: u64,
    /// Transactions that went to pending without a promotion (`queueTxEvent`), by sender and nonce.
    events: BTreeMap<Address, BTreeMap<u64, Tx>>,
    /// New executable transactions for the caller to send out; see [`Pool::take_new`].
    outbox: Vec<Tx>,
}

impl Pool {
    /// A pool at the head (`number`, `gas_limit`). `authority` says who may send special
    /// transactions; give [`Authority::Syncing`] until the node is synced.
    pub fn new(
        spec: ChainSpec,
        cfg: &Config,
        authority: Authority,
        number: u64,
        gas_limit: u64,
    ) -> Result<Self, Error> {
        let cfg = cfg.sanitized();
        let head = Head::new(&spec, number, gas_limit)?;
        Ok(Self {
            min_tip: u128::from(cfg.price_limit),
            spec,
            cfg,
            head,
            authority,
            pending: BTreeMap::new(),
            queue: Queue::default(),
            all: Lookup::default(),
            priced: Priced::default(),
            nonces: Noncer::default(),
            changes_since_reorg: 0,
            now: 0,
            events: BTreeMap::new(),
            outbox: Vec::new(),
        })
    }

    /// Who may send special transactions (`SetIsSigner`, `legacypool.go:317`).
    pub fn set_authority(&mut self, a: Authority) {
        self.authority = a;
    }

    pub fn head_number(&self) -> u64 {
        self.head.number
    }

    /// geth `SetGasTip`, `legacypool.go:442-463`: raising the minimum drops everything below it.
    pub fn set_gas_tip<S: StateRead>(&mut self, state: &S, tip: u128) {
        let old = self.min_tip;
        self.min_tip = tip;
        if tip > old {
            let drop: Vec<B256> = self
                .all
                .values()
                .filter(|t| t.tip_cap() < tip)
                .map(|t| t.hash)
                .collect();
            for h in &drop {
                self.remove_tx(state, *h, false);
            }
            self.priced.removed(drop.len(), &self.all);
        }
    }

    /// Transactions of the pool by hash.
    pub fn get(&self, hash: &B256) -> Option<&Arc<PoolTx>> {
        self.all.get(hash)
    }

    pub fn has(&self, hash: &B256) -> bool {
        self.all.get(hash).is_some()
    }

    /// `(size of the lookup in tx, in slots)`.
    pub fn len(&self) -> (usize, usize) {
        (self.all.count(), self.all.slots())
    }

    pub fn is_empty(&self) -> bool {
        self.all.count() == 0
    }

    /// geth `Stats`: executable and queued counts.
    pub fn stats(&self) -> (usize, usize) {
        (
            self.pending.values().map(List::len).sum(),
            self.queue.stats(),
        )
    }

    pub fn status(&self, hash: &B256) -> Status {
        let Some(tx) = self.all.get(hash) else {
            return Status::Unknown;
        };
        let (from, n) = (tx.from(), tx.tx.nonce);
        if self.pending.get(&from).is_some_and(|l| l.contains(n)) {
            Status::Pending
        } else if self.queue.get(&from).is_some_and(|l| l.contains(n)) {
            Status::Queued
        } else {
            Status::Unknown
        }
    }

    /// geth `Nonce`: the next nonce of `addr` with every executable transaction applied.
    pub fn nonce<S: StateRead>(&mut self, state: &S, addr: Address) -> u64 {
        self.nonces.get(state, addr)
    }

    /// geth `Content`: `(pending, queued)` by sender, each sorted by nonce.
    #[allow(clippy::type_complexity)]
    pub fn content(
        &self,
    ) -> (
        BTreeMap<Address, Vec<Arc<PoolTx>>>,
        BTreeMap<Address, Vec<Arc<PoolTx>>>,
    ) {
        let p = self
            .pending
            .iter()
            .filter(|(_, l)| !l.is_empty())
            .map(|(a, l)| (*a, l.flatten()))
            .collect();
        (p, self.queue.content())
    }

    /// The new executable transactions since the last call (geth's `NewTxsEvent`, which
    /// `BroadcastTransactions` consumes). Feed them to [`propagate::broadcast`].
    pub fn take_new(&mut self) -> Vec<Arc<PoolTx>> {
        std::mem::take(&mut self.outbox)
    }

    pub(crate) fn queue_tx_event(&mut self, tx: &Tx) {
        self.events
            .entry(tx.from())
            .or_default()
            .insert(tx.tx.nonce, tx.clone());
    }

    /// Hand the events and the promoted transactions to the outbox (end of `runReorg`,
    /// `legacypool.go:1684-1721`); `promoted` keeps only what truncation left in the pool (XDC-31).
    pub(crate) fn flush_events(&mut self, promoted: Vec<Tx>) {
        for t in promoted {
            if self.all.get(&t.hash).is_some() {
                self.events
                    .entry(t.from())
                    .or_default()
                    .insert(t.tx.nonce, t);
            }
        }
        for (_, m) in std::mem::take(&mut self.events) {
            self.outbox.extend(m.into_values());
        }
        // Bounded if the caller never drains it: the oldest go first, as in a peer's queue.
        let cap = usize::try_from(self.cfg.global_slots.saturating_add(self.cfg.global_queue))
            .unwrap_or(usize::MAX);
        if self.outbox.len() > cap {
            self.outbox.drain(..self.outbox.len() - cap);
        }
    }

    /// Drop events of nonces the new state has passed (`events[addr].Forward`, `legacypool.go:1625-1629`).
    pub(crate) fn forward_events<S: StateRead>(&mut self, state: &S) {
        let addrs: Vec<Address> = self.events.keys().copied().collect();
        for a in addrs {
            let n = self.nonces.get(state, a);
            if let Some(m) = self.events.get_mut(&a) {
                *m = m.split_off(&n);
                if m.is_empty() {
                    self.events.remove(&a);
                }
            }
        }
    }

    /// Every account with a pooled transaction.
    pub fn senders(&self) -> BTreeSet<Address> {
        self.pending
            .keys()
            .chain(self.queue.addresses().iter())
            .copied()
            .collect()
    }
}

/// `params.XDCBaseFeeForBlock` without the `Eip1559Block` gate (`params/xdc_gas.go:36-42`): the tier price
/// of block `number`. `CalcBaseFee` (the pending base fee) and the pool floor both read it.
pub(crate) fn tier_price(spec: &ChainSpec, number: u64) -> u128 {
    let base = u128::from(XDC_BASE_FEE);
    if spec.gas2500x.is_active(number) {
        base * u128::from(GAS_2500X_MULTIPLIER)
    } else {
        base
    }
}
