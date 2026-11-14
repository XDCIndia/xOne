//! The real block executor behind [`BlockExecutor`]: `xone_executor::execute_block` with the V1
//! chain context it reads (header window, sign transactions, deployed code, the V1 snapshot).
//! Specification: `spec/sync/import-export.md` section "Executor".
//!
//! The context is built from the blocks this executor has executed, and everything that cannot be
//! derived from a header (deployed code, sign transactions with their receipt status, the snapshot
//! after the gap reseed) travels in [`Execution::aux`], so that it is stored with the block in the
//! same commit (ADR-005 S5). [`XdcExecutor::restore`] rebuilds the context from those rows after
//! a restart. A block is added to the context only when its child arrives with it as parent, so a
//! block that failed to commit is never part of it.
//!
//! Only V1 blocks execute here (the V2 header context belongs to the follower, XB-11): a block
//! above the switch block is a typed error, never a different rule.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use std::{
    collections::BTreeMap,
    sync::{Mutex, PoisonError},
};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_consensus::{
    chain::{BlockReader, HeaderView, SignTx},
    v1::{checkpoint, snapshot::Snapshot},
    verify::{Body, Context, V1Context},
};
use xone_executor::{execute_block, Env, ExecCtx};
use xone_primitives::{Header, WideBalance};
use xone_state::{
    flat::{FlatWrite, MemFlat},
    system_contracts::validator_0x88 as v88,
    Account, Missing, StateDiff, StateRead,
};

use crate::{
    executor::{BlockExecutor, ExecError, Execution},
    store::Store,
};

/// Blocks of context kept behind the tip: the 4,500 headers `spec/retention.md` derives for the V1
/// reward window (five epochs) and the 256 of BLOCKHASH.
fn keep(spec: &ChainSpec) -> u64 {
    5 * spec.xdpos.epoch + 256
}

#[derive(RlpEncodable, RlpDecodable)]
struct CodeRow {
    hash: B256,
    code: Bytes,
}

#[derive(RlpEncodable, RlpDecodable)]
struct SignRow {
    sender: Address,
    signed: B256,
    ok: bool,
}

#[derive(RlpEncodable, RlpDecodable)]
struct RecentRow {
    number: u64,
    signer: Address,
}

#[derive(RlpEncodable, RlpDecodable)]
struct WideRow {
    address: Address,
    balance: Bytes,
}

/// What one block adds to the executor's context, as stored in `Record::aux`.
#[derive(RlpEncodable, RlpDecodable)]
struct Aux {
    codes: Vec<CodeRow>,
    signs: Vec<SignRow>,
    /// The V1 snapshot after the block (and its gap reseed).
    signers: Vec<Address>,
    recents: Vec<RecentRow>,
    /// Every account above 2^256 in the state after the block.
    wide: Vec<WideRow>,
}

#[derive(Clone)]
struct Stored {
    hash: B256,
    header: Header,
    miner: Address,
    signs: Vec<SignTx>,
}

struct Ctx {
    epoch: u64,
    blocks: BTreeMap<u64, Stored>,
    codes: BTreeMap<B256, Bytes>,
    wide: BTreeMap<Address, WideBalance>,
    /// The snapshot after the tip of `blocks`.
    snap: Snapshot,
    /// Analysed bytecode across blocks.
    code_cache: xone_executor::code_cache::CodeCache,
}

struct Pending {
    stored: Stored,
    codes: BTreeMap<B256, Bytes>,
    wide: BTreeMap<Address, WideBalance>,
    snap: Snapshot,
}

struct State {
    ctx: Ctx,
    pending: Option<Pending>,
}

impl BlockReader for Ctx {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        let s = self.blocks.get(&number).filter(|s| s.hash == hash)?;
        Some(HeaderView {
            number,
            hash,
            parent_hash: s.header.parent_hash,
            miner: s.miner,
            epoch_switch: false,
        })
    }
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>> {
        let s = self.blocks.get(&number).filter(|s| s.hash == hash)?;
        Some(s.signs.clone())
    }
}

impl Env for Ctx {
    fn hash_of(&self, number: u64) -> Option<B256> {
        Some(self.blocks.get(&number)?.hash)
    }
    fn masternodes_at(&self, checkpoint: u64) -> Option<Vec<Address>> {
        checkpoint::masternodes_of(&self.blocks.get(&checkpoint)?.header.extra_data).ok()
    }
    fn wide_balances(&self) -> Vec<(Address, WideBalance)> {
        self.wide.iter().map(|(a, w)| (*a, w.clone())).collect()
    }
    fn code(&self, hash: B256) -> Option<Bytes> {
        self.codes.get(&hash).cloned()
    }
    // V1 only: this context holds no V2 gap snapshots or burn records, so a V2 checkpoint block
    // is refused by the executor (`ExecError::NoBurn`, no standby pool) and never mis-paid.
    fn standby_pool(&self, _switch: u64) -> Option<Vec<Address>> {
        None
    }
    fn burn(&self, _number: u64) -> Option<alloy_primitives::U256> {
        None
    }
    fn code_cache(&self) -> Option<&xone_executor::code_cache::CodeCache> {
        Some(&self.code_cache)
    }
}

impl Ctx {
    /// geth `GetMasternodesWithParents` (`consensus/XDPoS/xdpos.go:2062-2083`): the list of the
    /// epoch of block `x`, from its checkpoint header; `own` is `x`'s header when not stored yet.
    fn epoch_list(&self, x: u64, own: &Header) -> Result<Vec<Address>, ExecError> {
        let cp = x - x % self.epoch;
        let h = if cp == own.number {
            own
        } else {
            &self
                .blocks
                .get(&cp)
                .ok_or_else(|| ExecError::Failed(format!("no checkpoint header {cp}")))?
                .header
        };
        checkpoint::masternodes_of(&h.extra_data).map_err(|e| ExecError::Failed(e.to_string()))
    }

    /// The V1 inputs of the header verifier for `header`, whose parent is the tip.
    fn v1_context(&self, header: &Header) -> Result<Context, ExecError> {
        let n = header.number;
        let cp = n - n % self.epoch;
        let checkpoint = if cp == n {
            None
        } else {
            Some(
                self.blocks
                    .get(&cp)
                    .ok_or_else(|| ExecError::Failed(format!("no checkpoint header {cp}")))?
                    .header
                    .clone(),
            )
        };
        Ok(Context::V1(Box::new(V1Context {
            masternodes: self.epoch_list(n, header)?,
            parent_masternodes: self.epoch_list(n - 1, header)?,
            signers: self.snap.get_signers(),
            checkpoint,
            // only read where geth runs the full V1 verification, which it does not on Apothem
            checkpoint_signers: vec![],
        })))
    }

    fn push(&mut self, p: Pending, keep: u64) {
        let number = p.stored.header.number;
        self.blocks.insert(number, p.stored);
        self.codes.extend(p.codes);
        self.wide = p.wide;
        self.snap = p.snap;
        let floor = number.saturating_sub(keep);
        self.blocks = self.blocks.split_off(&floor);
    }
}

/// The state after `diff` on top of `base`, for the gap reseed.
struct After<'a>(&'a dyn StateRead, &'a StateDiff);

impl StateRead for After<'_> {
    fn account(&self, a: Address) -> Option<Account> {
        match self.1.accounts.get(&a) {
            Some(x) => x.clone(),
            None => self.0.account(a),
        }
    }
    fn storage(&self, a: Address, k: B256) -> U256 {
        if let Some(v) = self.1.storage.get(&(a, k)) {
            return *v;
        }
        match self.1.accounts.get(&a) {
            Some(None) => U256::ZERO, // a deleted account takes its storage with it
            _ => self.0.storage(a, k),
        }
    }
}

/// A sized handle for the `&dyn StateRead` the chain manager passes.
struct Parent<'a>(&'a dyn StateRead);

impl StateRead for Parent<'_> {
    fn account(&self, a: Address) -> Option<Account> {
        self.0.account(a)
    }
    fn storage(&self, a: Address, k: B256) -> U256 {
        self.0.storage(a, k)
    }
    fn root_with(&self, diff: &StateDiff) -> Result<B256, Missing> {
        self.0.root_with(diff)
    }
}

/// geth `updateM1At` (`core/blockchain.go:3963-3990`) and `UpdateMasternodesV1`
/// (`consensus/XDPoS/xdpos.go:2135-2150`, `:2156-2174`): at a V1 gap block the snapshot's signers
/// become the cap-sorted candidates of 0x88 in that block's state.
fn reseed(
    spec: &ChainSpec,
    snap: &mut Snapshot,
    state: &impl StateRead,
    n: u64,
) -> Result<(), ExecError> {
    let x = &spec.xdpos;
    let at_gap = x.gap != 0 && x.gap <= x.epoch && n % x.epoch == x.epoch - x.gap;
    let v1 = x.v2.as_ref().is_none_or(|v| n <= v.switch_block);
    if !(at_gap && v1) {
        return Ok(());
    }
    let cands: Vec<_> = v88::candidates(state)
        .into_iter()
        .map(|a| (a, v88::candidate_cap(state, a)))
        .collect();
    let tip = match spec.tip_increase_masternodes {
        ForkBlock::At(b) => Some(b),
        _ => None,
    };
    let max = checkpoint::max_masternodes(
        n,
        tip,
        x.v2.as_ref().map(|v| v.switch_block),
        x.max_masternodes_v2,
    );
    snap.reseed(
        checkpoint::select_candidates(&cands, max).map_err(|e| ExecError::Failed(e.to_string()))?,
    );
    Ok(())
}

fn fail(e: impl std::fmt::Display) -> ExecError {
    ExecError::Failed(e.to_string())
}

/// The genesis state of `net` and its deployed code, from the alloc.
pub fn genesis_state(net: Network) -> Result<(MemFlat, BTreeMap<B256, Bytes>), ExecError> {
    let g = net.genesis().map_err(fail)?;
    let mut flat = MemFlat::default();
    let mut codes = BTreeMap::new();
    for (a, acc) in &g.alloc {
        let code_hash = if acc.code.is_empty() {
            alloy_primitives::KECCAK256_EMPTY
        } else {
            let h = alloy_primitives::keccak256(&acc.code);
            codes.insert(h, acc.code.clone());
            h
        };
        flat.set_account(
            *a,
            Some(Account {
                nonce: acc.nonce,
                balance: WideBalance::from(acc.balance),
                code_hash,
                ..Default::default()
            }),
        );
        for (k, v) in &acc.storage {
            flat.set_slot(*a, *k, U256::from_be_bytes(v.0));
        }
    }
    Ok((flat, codes))
}

/// The genesis block of `net`: header, state and deployed code, after the hash and root checks.
pub fn genesis(net: Network) -> Result<(Header, MemFlat, BTreeMap<B256, Bytes>), ExecError> {
    let spec = net.spec();
    let (flat, codes) = genesis_state(net)?;
    let header = net
        .genesis()
        .map_err(fail)?
        .to_header(&spec)
        .map_err(fail)?;
    if header.hash() != spec.genesis_hash
        || xone_state::trie::flat_root(&flat) != spec.genesis_state_root
    {
        return Err(fail(
            "genesis hash or state root differs from the chain spec",
        ));
    }
    Ok((header, flat, codes))
}

pub struct XdcExecutor {
    spec: ChainSpec,
    st: Mutex<State>,
}

impl XdcExecutor {
    /// An executor whose context is the genesis block of `net`.
    pub fn new(net: Network) -> Result<Self, ExecError> {
        let (g, _, codes) = genesis(net)?;
        Self::from_genesis(net.spec(), g, codes)
    }

    /// An executor whose context is the genesis block `g` of a chain with rules `spec`.
    pub fn from_genesis(
        spec: ChainSpec,
        g: Header,
        codes: BTreeMap<B256, Bytes>,
    ) -> Result<Self, ExecError> {
        let snap = Snapshot::new(
            0,
            g.hash(),
            checkpoint::masternodes_of(&g.extra_data).map_err(fail)?,
        );
        let stored = Stored {
            hash: g.hash(),
            header: g,
            miner: Address::ZERO,
            signs: vec![],
        };
        Ok(Self::with(
            spec.clone(),
            Ctx {
                epoch: spec.xdpos.epoch,
                blocks: BTreeMap::from([(0, stored)]),
                codes,
                wide: BTreeMap::new(),
                snap,
                code_cache: xone_executor::code_cache::CodeCache::default(),
            },
        ))
    }

    fn with(spec: ChainSpec, ctx: Ctx) -> Self {
        Self {
            spec,
            st: Mutex::new(State { ctx, pending: None }),
        }
    }

    /// Rebuilds the context after a restart from the rows stored with the canonical blocks of
    /// `store` (the head and its window; the code of every block). The genesis code comes from
    /// the network's genesis.
    pub fn restore<S: Store>(net: Network, store: &S) -> Result<Self, ExecError> {
        let me = Self::new(net)?;
        let (head, head_hash) = store.head();
        {
            let mut st = me.st.lock().unwrap_or_else(PoisonError::into_inner);
            let floor = head.saturating_sub(keep(&me.spec));
            for n in 1..=head {
                let h = store
                    .canonical_hash(n)
                    .ok_or_else(|| fail(format!("no canonical block {n}")))?;
                let rec = store
                    .record(h)
                    .ok_or_else(|| fail(format!("no record for block {n}")))?;
                let aux: Aux = alloy_rlp::decode_exact(&rec.aux[..])
                    .map_err(|e| fail(format!("aux of block {n}: {e}")))?;
                st.ctx
                    .codes
                    .extend(aux.codes.into_iter().map(|c| (c.hash, c.code)));
                if n >= floor {
                    let miner = rec.block.header.v1_signer().map_err(fail)?;
                    st.ctx.blocks.insert(
                        n,
                        Stored {
                            hash: h,
                            header: rec.block.header.clone(),
                            miner,
                            signs: aux
                                .signs
                                .iter()
                                .map(|s| SignTx {
                                    sender: s.sender,
                                    signed: s.signed,
                                    ok: s.ok,
                                })
                                .collect(),
                        },
                    );
                }
                if n == head {
                    let mut snap = Snapshot::new(n, h, aux.signers);
                    snap.recents = aux.recents.iter().map(|r| (r.number, r.signer)).collect();
                    st.ctx.snap = snap;
                    st.ctx.wide = aux
                        .wide
                        .iter()
                        .map(|w| (w.address, WideBalance::from_be_bytes(&w.balance)))
                        .collect();
                }
            }
            if head > 0 && st.ctx.blocks.get(&head).map(|s| s.hash) != Some(head_hash) {
                return Err(fail("the restored context does not end at the head"));
            }
        }
        Ok(me)
    }
}

impl BlockExecutor for XdcExecutor {
    fn execute(
        &self,
        parent: &dyn StateRead,
        header: &Header,
        body: &Body,
    ) -> Result<Execution, ExecError> {
        let n = header.number;
        if self
            .spec
            .xdpos
            .v2
            .as_ref()
            .is_some_and(|v| n > v.switch_block)
        {
            return Err(fail(format!(
                "block {n} is above the V2 switch block: this executor has no V2 header context"
            )));
        }
        let mut st = self.st.lock().unwrap_or_else(PoisonError::into_inner);
        let keep = keep(&self.spec);
        if let Some(p) = st.pending.take() {
            if p.stored.hash == header.parent_hash {
                st.ctx.push(p, keep);
            }
        }
        let ctx = &st.ctx;
        let parent_stored = ctx
            .blocks
            .get(&(n.wrapping_sub(1)))
            .filter(|s| s.hash == header.parent_hash && ctx.snap.hash == s.hash)
            .ok_or_else(|| {
                fail(format!(
                    "the executor context ends at snapshot {} {}, block {n} needs parent {}",
                    ctx.snap.number, ctx.snap.hash, header.parent_hash
                ))
            })?;
        let verify = ctx.v1_context(header)?;
        let view = Parent(parent);
        let o = execute_block(
            &self.spec,
            &view,
            header,
            body,
            &ExecCtx {
                parent: &parent_stored.header,
                verify: &verify,
                env: ctx,
                derive: None,
            },
        )
        .map_err(fail)?;
        let mut snap = ctx
            .snap
            .apply(std::slice::from_ref(header), self.spec.xdpos.epoch)
            .map_err(fail)?;
        reseed(&self.spec, &mut snap, &After(parent, &o.diff), n)?;
        if !snap.votes.is_empty() || !snap.tally.is_empty() {
            return Err(fail("a snapshot with votes in flight cannot be stored"));
        }
        let mut wide = ctx.wide.clone();
        for (a, acc) in &o.diff.accounts {
            match acc {
                Some(x) if x.balance.is_wide() => wide.insert(*a, x.balance.clone()),
                _ => wide.remove(a),
            };
        }
        let aux = alloy_rlp::encode(Aux {
            codes: o
                .codes
                .iter()
                .map(|(h, c)| CodeRow {
                    hash: *h,
                    code: c.clone(),
                })
                .collect(),
            signs: o
                .signs
                .iter()
                .map(|s| SignRow {
                    sender: s.sender,
                    signed: s.signed,
                    ok: s.ok,
                })
                .collect(),
            signers: snap.get_signers(),
            recents: snap
                .recents
                .iter()
                .map(|(n, s)| RecentRow {
                    number: *n,
                    signer: *s,
                })
                .collect(),
            wide: wide
                .iter()
                .map(|(a, w)| WideRow {
                    address: *a,
                    balance: w.to_be_bytes().into(),
                })
                .collect(),
        });
        let stored = Stored {
            hash: o.hash,
            header: header.clone(),
            miner: header.v1_signer().map_err(fail)?,
            signs: o.signs,
        };
        let ex = Execution {
            diff: o.diff,
            receipts: o.receipt_encodings,
            state_root: o.state_root,
            gas_used: o.gas_used,
            access_list_hash: None,
            aux: aux.into(),
        };
        st.pending = Some(Pending {
            stored,
            codes: o.codes,
            wide,
            snap,
        });
        Ok(ex)
    }
}
