//! The chain as the node runs it: `xone_sync::chain::Chain` over the [`NodeStore`] (MDBX and the
//! segment files, ADR-005), the executor adapter (`xone_executor::execute_verified`) and the header
//! check adapter (`HeaderVerifier`), both reading the node's own [`History`]. A restart reconciles
//! the store, unwinds it to the newest state checkpoint and replays every block above it through the
//! same `Chain::insert`, so nothing that was stored is trusted without being verified and executed
//! again.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use tokio::sync::watch;
use xone_chainspec::{ChainSpec, Genesis};
use xone_consensus::{
    chain::SignTx,
    v1::{checkpoint::masternodes_of, snapshot::Snapshot},
    v2::gap_snapshot,
    verify::{Body, HeaderError, HeaderVerifier},
};
use xone_db::mdbx::T;
use xone_executor::execute_verified;
use xone_primitives::{Header, WideBalance};
use xone_rpc::subscriptions::Hub;
use xone_state::{
    trie::flat_root, Account, FlatRead, FlatWrite, MemFlat, Missing, StateDiff, StateRead,
};
use xone_sync::{
    bootstrap::{
        minimal::{self, Held},
        Class, Retention,
    },
    chain::{
        insert::Outcome,
        resume::{resume, ResumeError, Resumed},
        Chain, ChainRead, HeaderCheck, InsertError,
    },
    executor::{BlockExecutor, ExecError, Execution},
    store::{Block, Store, StoreError, Unwound},
};

use super::{
    hist::{
        gap_row, reseed, switch_of_row, switch_row, window_of, HistInner, Stored, MEMORY_WINDOW,
    },
    senders::Senders,
    store::{self, Ckpt, FileError, NodeStore},
};

/// Minimal mode deletes when the floor has moved this many blocks past the first held block: a
/// rewrite of the segment files every 256 blocks, not every block.
pub const PRUNE_SLACK: u64 = 256;

/// Blocks above the anchor whose state can be opened (RPC reads at a block, unwind window, the roots
/// a node serves over snap/1). 4096: a snap client needs the 0x88 state of every gap block up to
/// `3E + G - 1` (3,149 on every XDC network) below its pivot, and its pivot 64 below the head of the
/// node that serves it (ADR-005 amendment 2026-11-12, `spec/snap/pivot.md`).
pub const RETENTION: u64 = 4096;

/// ponytail: group commit of a node that does not sign. The block commits of this interval become
/// steady together: one sync of each touched segment chunk, the segment directories and MDBX, instead
/// of a dozen syncs per block (about 30 ms each on a busy disk, 3 blocks per second). A crash of the
/// system loses at most this interval, which is fetched again. Shorter if that is too much.
pub const GROUP_COMMIT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    File(#[from] FileError),
    #[error("genesis: {0}")]
    Genesis(String),
    #[error("refused to resume: {0}")]
    Resume(#[from] ResumeError),
    #[error("block {number}: {error}")]
    Insert { number: u64, error: InsertError },
    #[error("{0}")]
    Inconsistent(String),
    #[error("the datadir was bootstrapped from a snapshot and has no history below its anchor: start it with --minimal")]
    Anchored,
    #[error("--minimal starts from the anchor of an imported snapshot, and this datadir has none: run `xone snapshot import` first (a minimal node never syncs from genesis or reads an archive)")]
    NotAnchored,
    #[error("minimal mode: {0}")]
    Minimal(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("the datadir was written by a build without the history tables (TxLookup, ConsensusEpoch, ConsensusSnapshot): remove it and sync again")]
    NoHistoryTables,
}

/// The state as a sized reader, so the generic executor and the pool can take a `&dyn StateRead`.
pub struct Dyn<'a>(pub &'a dyn StateRead);

impl StateRead for Dyn<'_> {
    fn account(&self, address: Address) -> Option<Account> {
        self.0.account(address)
    }
    fn storage(&self, address: Address, slot: B256) -> U256 {
        self.0.storage(address, slot)
    }
    fn root_with(&self, diff: &StateDiff) -> Result<B256, Missing> {
        self.0.root_with(diff)
    }
}

struct Pending {
    hash: B256,
    snap: Option<Snapshot>,
    signs: Vec<SignTx>,
    codes: BTreeMap<B256, Bytes>,
    wide: Vec<(Address, Option<WideBalance>)>,
    receipts: Vec<Bytes>,
}

pub struct History {
    pub inner: RwLock<HistInner>,
    pending: Mutex<Option<Pending>>,
}

impl History {
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, HistInner> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HistInner> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }
    fn pending(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `BlockExecutor` over `execute_verified`: the block must extend the tip of the node's history.
/// `Chain::insert` runs [`NodeCheck`] on the header (with its QC) before it executes the block, so
/// the header is verified once per block, as geth's `insertChain` does; the senders come from the
/// node's [`Senders`] workers.
pub struct NodeExec {
    spec: ChainSpec,
    hist: std::sync::Arc<History>,
    senders: std::sync::Arc<Senders>,
}

impl BlockExecutor for NodeExec {
    fn execute(
        &self,
        parent: &dyn StateRead,
        header: &Header,
        body: &Body,
    ) -> Result<Execution, ExecError> {
        let fail = |e: String| ExecError::Failed(e);
        let senders = self.senders.of(&body.transactions);
        let h = self.hist.read();
        let tip = h.tip();
        if header.parent_hash != tip.hash || header.number != h.tip_number() + 1 {
            return Err(fail(format!(
                "block {} does not extend the tip {}: side branches are not executed yet",
                header.number,
                h.tip_number()
            )));
        }
        let snap = h.snaps.get(&tip.header.number);
        let out = execute_verified(
            &self.spec,
            &Dyn(parent),
            header,
            body,
            senders.as_deref(),
            &tip.header,
            &*h,
        )
        .map_err(|e| fail(e.to_string()))?;
        let v1 = self
            .spec
            .xdpos
            .v2
            .as_ref()
            .is_none_or(|v| header.number <= v.switch_block);
        let next = match (v1, snap) {
            (true, Some(s)) => Some(
                s.apply(std::slice::from_ref(header), self.spec.xdpos.epoch)
                    .map_err(|e| fail(e.to_string()))?,
            ),
            _ => None,
        };
        let wide = out
            .diff
            .accounts
            .iter()
            .map(|(a, acc)| {
                (
                    *a,
                    acc.as_ref()
                        .filter(|x| x.balance.is_wide())
                        .map(|x| x.balance.clone()),
                )
            })
            .collect();
        *self.hist.pending() = Some(Pending {
            hash: out.hash,
            snap: next,
            signs: out.signs,
            codes: out.codes,
            wide,
            receipts: out.receipt_encodings.clone(),
        });
        Ok(Execution {
            diff: out.diff,
            receipts: out.receipt_encodings,
            state_root: out.state_root,
            gas_used: out.gas_used,
            access_list_hash: out.bal_hash,
            aux: Bytes::new(),
        })
    }
}

/// `HeaderCheck` over `HeaderVerifier`, with the context of the node's history.
pub struct NodeCheck {
    spec: ChainSpec,
    hist: std::sync::Arc<History>,
}

impl HeaderCheck for NodeCheck {
    fn check(
        &self,
        _chain: &dyn ChainRead,
        header: &Header,
        parent: &Header,
        now: u64,
    ) -> Result<(), HeaderError> {
        let h = self.hist.read();
        if parent.hash() != h.tip().hash {
            return Err(HeaderError::UnknownAncestor(parent.number));
        }
        let ctx = h.context(header, parent, h.snaps.get(&parent.number))?;
        HeaderVerifier::new(&self.spec).verify(header, parent, now, &ctx)
    }
}

pub type NodeChain = Chain<NodeStore, NodeExec, NodeCheck>;

pub struct Core {
    pub spec: ChainSpec,
    pub chain: NodeChain,
    pub hist: std::sync::Arc<History>,
    pub dir: PathBuf,
    pub genesis: B256,
    /// Reads committed bodies and receipts of old blocks with no store or import lock ([`Core::records`]).
    reader: super::hist::Src,
    ckpt_every: u64,
    pub head: watch::Sender<(u64, B256)>,
    /// Started from an imported snapshot: prunes below the retention floor, serves nothing below it.
    pub minimal: bool,
    /// Blocks the retention floor moves before the store is rewritten ([`PRUNE_SLACK`]; a test sets 0).
    pub prune_slack: u64,
    /// Recovers the senders of bodies as they arrive, ahead of their import.
    pub senders: std::sync::Arc<Senders>,
    /// The state checkpoint being written ([`Self::checkpoint_in_background`]).
    saving: Mutex<Option<std::thread::JoinHandle<Result<(), FileError>>>>,
    /// The subscription hubs of the WebSocket and IPC endpoints: each canonical block and each block
    /// a reorg removes is published to them ([`Self::publish`]).
    pub hubs: Mutex<Vec<std::sync::Arc<Hub>>>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Genesis state, code and header, checked against each other.
fn genesis_state(
    g: &Genesis,
    spec: &ChainSpec,
) -> Result<(MemFlat, BTreeMap<B256, Bytes>, Header), CoreError> {
    let e = |e: xone_chainspec::Error| CoreError::Genesis(e.to_string());
    let mut flat = MemFlat::recording();
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
    let header = g.to_header(spec).map_err(e)?;
    if flat_root(&flat) != header.state_root {
        return Err(CoreError::Genesis(
            "the genesis alloc does not give the genesis state root".into(),
        ));
    }
    Ok((flat, codes, header))
}

/// The seal signer (V1) or the coinbase (V2) of `h`; zero for the genesis block.
pub(crate) fn miner_of(spec: &ChainSpec, h: &Header) -> Result<Address, CoreError> {
    if h.number == 0 {
        return Ok(Address::ZERO);
    }
    if spec
        .xdpos
        .v2
        .as_ref()
        .is_some_and(|v| h.number > v.switch_block)
    {
        return Ok(h.coinbase);
    }
    h.v1_signer()
        .map_err(|e| CoreError::Inconsistent(format!("block {}: {e}", h.number)))
}

/// Writes `c`, then removes the checkpoints below `keep_from`.
fn save(dir: &Path, c: &Ckpt, keep_from: u64) -> Result<(), FileError> {
    store::save_ckpt(dir, c)?;
    store::drop_ckpts_below(dir, keep_from);
    Ok(())
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Err(e) = self.join_save() {
            eprintln!("chain: {e}");
        }
        if let Err(e) = self.chain.with_store_mut(NodeStore::steady) {
            eprintln!("store: the last blocks are not steady on close: {e}");
        }
    }
}

impl Core {
    /// Opens `<dir>`: genesis on a new datadir, else the checkpoint and the stored blocks above it,
    /// replayed and verified. Refuses a datadir whose files do not fit `genesis`.
    pub fn open(
        spec: ChainSpec,
        genesis: &Genesis,
        dir: &Path,
        ckpt_every: u64,
    ) -> Result<Self, CoreError> {
        Self::open_with(spec, genesis, dir, ckpt_every, false, false)
    }

    /// [`Core::open`], and with `minimal` the start of a node from the anchor of an imported snapshot
    /// (`spec/bootstrap/minimal-mode.md`): the datadir must be anchored, holds nothing below its
    /// first retained block, and prunes to the derived retention floor as the head moves. An
    /// anchored datadir is never opened as a full node, and a full datadir never as a minimal one.
    /// `group_commit` (a node that does not sign) makes the block commits steady once per
    /// [`GROUP_COMMIT`] instead of each on the device before it returns (`xone_sync::mdbx::Config`).
    pub fn open_with(
        spec: ChainSpec,
        genesis: &Genesis,
        dir: &Path,
        ckpt_every: u64,
        minimal: bool,
        group_commit: bool,
    ) -> Result<Self, CoreError> {
        Self::open_full(spec, genesis, dir, ckpt_every, minimal, group_commit, false)
    }

    /// [`Core::open_with`], and with `disk_state` (`--state.disk`) the head state is read from the
    /// store's tables behind bounded caches: no checkpoint carries state rows, and a start reads the
    /// tables instead of a checkpoint's copy (`docs/ops/disk-state.md`).
    pub fn open_full(
        spec: ChainSpec,
        genesis: &Genesis,
        dir: &Path,
        ckpt_every: u64,
        minimal: bool,
        group_commit: bool,
        disk_state: bool,
    ) -> Result<Self, CoreError> {
        let (flat0, codes0, g) = genesis_state(genesis, &spec)?;
        std::fs::create_dir_all(dir).map_err(|e| FileError(format!("{}: {e}", dir.display())))?;
        let mut node_store = store::open(
            dir,
            RETENTION,
            group_commit.then_some(GROUP_COMMIT),
            disk_state,
        )?;
        let resumed = resume(&mut node_store)?;
        let loaded = store::load(&node_store)?;
        let anchored = loaded.as_ref().is_some_and(|l| l.anchor.is_some());
        match (anchored, minimal) {
            (true, false) => return Err(CoreError::Anchored),
            (false, true) => return Err(CoreError::NotAnchored),
            _ => {}
        }
        match resumed {
            Resumed::Fresh => node_store
                .init_anchor(g.clone(), flat0.clone())
                .map_err(|e| CoreError::Inconsistent(e.to_string()))?,
            Resumed::Head { .. } if !anchored && node_store.canonical_hash(0) != Some(g.hash()) => {
                return Err(CoreError::Inconsistent(
                    "the stored chain does not start at this genesis".into(),
                ));
            }
            Resumed::Head { .. } => {}
        }
        if loaded.is_some() && !node_store.has_history_tables()? {
            return Err(CoreError::NoHistoryTables);
        }
        let ck = loaded.as_ref().and_then(|l| {
            store::load_ckpt(dir, l.number, |c| {
                c.number >= l.first && node_store.canonical_hash(c.number) == Some(c.hash)
            })
        });
        let window = window_of(&spec);
        let header = |n: u64| {
            node_store.canonical_header(n)?.ok_or_else(|| {
                CoreError::Inconsistent(format!("block {n} is missing from the store"))
            })
        };
        let (start, flat, mut inner) = match (&ck, &loaded) {
            (Some(c), Some(l)) => {
                let mut snap = Snapshot::new(c.number, c.hash, c.signers.clone());
                snap.recents = c.recents.clone();
                let gaps = c
                    .gaps
                    .iter()
                    .map(|(n, v)| {
                        (
                            *n,
                            gap_snapshot::SnapshotV2 {
                                number: *n,
                                candidates: v.clone(),
                            },
                        )
                    })
                    .collect();
                // The window below the checkpoint: headers from the store, sign transactions from the
                // checkpoint (it holds those of its own window, from `signs_from`), bodies and
                // receipts for the memory window only. A minimal node holds nothing below its first
                // retained header.
                let first = if anchored { l.first } else { 0 };
                let from = first
                    .max(c.signs_from)
                    .max((c.number + 1).saturating_sub(window));
                let mut inner = HistInner {
                    spec: spec.clone(),
                    base: from,
                    blocks: VecDeque::new(),
                    window,
                    src: Some(node_store.reader()),
                    codes: c.codes.clone(),
                    wide: BTreeMap::new(),
                    snaps: BTreeMap::from([(c.number, snap)]),
                    gaps,
                    switches: vec![],
                    td: U256::ZERO,
                };
                for n in from..=c.number {
                    let header = header(n)?;
                    let (body, receipts) = if n + MEMORY_WINDOW > c.number {
                        store::records(&node_store, l, n)?
                    } else {
                        (Body::default(), vec![])
                    };
                    let s = Stored {
                        hash: header.hash(),
                        miner: miner_of(&spec, &header)?,
                        signs: c.signs.get(&n).map_or(vec![], |v| {
                            v.iter()
                                .map(|(sender, signed, ok)| SignTx {
                                    sender: *sender,
                                    signed: *signed,
                                    ok: *ok,
                                })
                                .collect()
                        }),
                        header,
                        body,
                        receipts,
                    };
                    inner.index(&s, n);
                    inner.td += s.header.difficulty;
                    inner.blocks.push_back(s);
                }
                if !anchored || !c.td.is_zero() {
                    // The history starts inside the chain: the checkpoint carries the total
                    // difficulty (an anchored one since the snap install gives the pivot's, #119).
                    // A checkpoint of a snapshot file has none: the window's own sum is a lower
                    // bound, and XDPoS peers rank by block number and round, not by difficulty.
                    inner.td = c.td;
                }
                // the switches below the window are rows of `ConsensusEpoch`
                let mut switches = Vec::new();
                for (n, row) in node_store.reader().rows(T::ConsensusEpoch)? {
                    if n >= from {
                        break;
                    }
                    switches.push(switch_of_row(n, &row).ok_or_else(|| {
                        CoreError::Inconsistent(format!("the epoch switch row of block {n}"))
                    })?);
                }
                switches.append(&mut inner.switches);
                inner.switches = switches;
                // with disk state the rows are the store's, not the checkpoint's
                let flat = if disk_state {
                    MemFlat::default()
                } else if c.haccounts.is_empty() && c.accounts.is_empty() {
                    return Err(CoreError::Inconsistent(format!(
                        "the state checkpoint of block {} holds no state rows (written with --state.disk): start with --state.disk",
                        c.number
                    )));
                } else {
                    c.flat()
                };
                (c.number, flat, inner)
            }
            _ => {
                let snap = Snapshot::new(
                    0,
                    g.hash(),
                    masternodes_of(&g.extra_data).map_err(|e| CoreError::Genesis(e.to_string()))?,
                );
                let inner = HistInner {
                    spec: spec.clone(),
                    base: 0,
                    blocks: VecDeque::from([Stored {
                        hash: g.hash(),
                        header: g.clone(),
                        miner: Address::ZERO,
                        signs: vec![],
                        body: Body::default(),
                        receipts: vec![],
                    }]),
                    window,
                    src: Some(node_store.reader()),
                    codes: codes0,
                    wide: BTreeMap::new(),
                    snaps: BTreeMap::from([(0, snap)]),
                    gaps: BTreeMap::new(),
                    switches: vec![],
                    td: g.difficulty,
                };
                (
                    0,
                    if disk_state {
                        MemFlat::default()
                    } else {
                        flat0
                    },
                    inner,
                )
            }
        };
        // Accounts above 2^256 are tracked by address (`Env::wide_balances`); the flat state holds only
        // hashes, so each address comes from the preimage table (filled by execution, genesis and
        // installs). An unknown one is an error, never a silently narrowed balance.
        let track_wide = |inner: &mut HistInner,
                          node_store: &store::NodeStore,
                          wide: Vec<(B256, Account)>|
         -> Result<(), CoreError> {
            for (ha, a) in wide {
                let addr = node_store
                    .preimage(ha)
                    .filter(|p| p.len() == 20)
                    .map(|p| Address::from_slice(&p))
                    .ok_or_else(|| {
                        CoreError::Inconsistent(format!(
                            "the account with key {ha} holds more than 2^256 and its address is not in this datadir"
                        ))
                    })?;
                inner.wide.insert(addr, a.balance);
            }
            Ok(())
        };
        let anchor = inner.tip().header.clone();
        if !disk_state {
            track_wide(
                &mut inner,
                &node_store,
                flat.hashed_accounts()
                    .filter(|(_, a)| a.balance.is_wide())
                    .collect(),
            )?;
            if flat_root(&flat) != anchor.state_root {
                return Err(CoreError::Inconsistent(format!(
                    "the state checkpoint of block {start} does not give its state root"
                )));
            }
        }
        if anchored && ck.is_none() {
            return Err(CoreError::Inconsistent(
                "an anchored datadir without a state checkpoint at or below its head".into(),
            ));
        }
        let hist = std::sync::Arc::new(History {
            inner: RwLock::new(inner),
            pending: Mutex::new(None),
        });
        // The blocks above the checkpoint are read before the store goes back to it, and replayed
        // below.
        let replay = match &loaded {
            Some(l) => (start + 1..=l.number)
                .map(|n| Ok((header(n)?, store::records(&node_store, l, n)?.0)))
                .collect::<Result<Vec<_>, CoreError>>()?,
            None => vec![],
        };
        // A minimal node holds what its head needs: the head the datadir stopped at, not the
        // checkpoint the store goes back to for the replay (that one is up to a checkpoint interval
        // behind, and needs a longer history than a snapshot of a later pivot holds).
        if minimal {
            minimal::check_start(&spec, &node_store)
                .map_err(|e| CoreError::Minimal(e.to_string()))?;
        }
        if loaded.as_ref().is_some_and(|l| start < l.number) {
            node_store.unwind_to(start, Unwound::Drop).map_err(|e| {
                CoreError::Inconsistent(format!(
                    "no state checkpoint within the last {RETENTION} blocks to replay from: {e}"
                ))
            })?;
        }
        if disk_state {
            // the state of `start` is the store's: its trie root is the check, the wide accounts
            // are read from the tables
            let wide = node_store.wide_accounts();
            track_wide(&mut hist.write(), &node_store, wide)?;
        }
        let root = if disk_state {
            node_store.state_root()
        } else {
            node_store.flat_root()
        };
        if node_store.head() != (start, anchor.hash()) || root != anchor.state_root {
            return Err(CoreError::Inconsistent(format!(
                "the store at block {start} is not the checkpoint's state"
            )));
        }
        let senders = std::sync::Arc::new(Senders::new(spec.chain_id));
        let exec = NodeExec {
            spec: spec.clone(),
            hist: hist.clone(),
            senders: senders.clone(),
        };
        let check = NodeCheck {
            spec: spec.clone(),
            hist: hist.clone(),
        };
        let reader = node_store.reader();
        let chain = Chain::new(spec.clone(), node_store, exec, check);
        let (head, _) = watch::channel((start, hist.read().tip().hash));
        let core = Self {
            spec,
            chain,
            hist,
            dir: dir.to_owned(),
            genesis: g.hash(),
            reader,
            ckpt_every,
            head,
            minimal,
            prune_slack: PRUNE_SLACK,
            senders,
            saving: Mutex::new(None),
            hubs: Mutex::default(),
        };
        for (header, body) in replay {
            let number = header.number;
            match core.import(header, body, now())? {
                Outcome::Extended => {}
                o => {
                    return Err(CoreError::Inconsistent(format!(
                        "stored block {number} replayed as {o:?}"
                    )))
                }
            }
        }
        if let Some(l) = loaded {
            if core.chain.head() != (l.number, l.hash) {
                return Err(CoreError::Inconsistent(format!(
                    "the replay ends at {:?}, the store's head was {} {}",
                    core.chain.head(),
                    l.number,
                    l.hash
                )));
            }
        }
        Ok(core)
    }

    /// Imports one block: header check, execution, root comparison and the store commit. `Extended`
    /// adds it to the history.
    pub fn import(&self, header: Header, body: Body, now: u64) -> Result<Outcome, CoreError> {
        let block = Block::new(header, body);
        let (hash, number) = (block.hash(), block.number());
        let body = block.body.clone();
        let header = block.header.clone();
        *self.hist.pending() = None;
        let out = self
            .chain
            .insert(block, now)
            .map_err(|error| CoreError::Insert { number, error })?;
        if out != Outcome::Extended {
            return Ok(out);
        }
        // The block is committed. If its history context cannot be built, rewind the chain to the
        // parent so the store head and the history tip stay the same block.
        let derived = (|| -> Result<_, CoreError> {
            let mut p = self
                .hist
                .pending()
                .take()
                .filter(|p| p.hash == hash)
                .ok_or_else(|| {
                    CoreError::Inconsistent(format!("no execution record for {hash}"))
                })?;
            let (mut snap, mut gap, mut err) = (p.snap.take(), None, None);
            let want_gap = self.hist.read().is_gap_block(number);
            self.chain
                .reader()
                .with_state(hash, &mut |s| {
                    if let Some(sn) = snap.as_mut() {
                        err = reseed(&self.spec, sn, &Dyn(s), number).err();
                    }
                    if want_gap && err.is_none() {
                        match gap_snapshot::build(&Dyn(s), number) {
                            Ok(g) => gap = Some(g),
                            Err(e) => err = Some(e.to_string()),
                        }
                    }
                })
                .map_err(|e| CoreError::Inconsistent(e.to_string()))?;
            if let Some(e) = err {
                return Err(CoreError::Inconsistent(format!("block {number}: {e}")));
            }
            let miner = miner_of(&self.spec, &header)?;
            Ok((p, snap, gap, miner))
        })();
        let (p, snap, gap, miner) = derived.inspect_err(|_| {
            let _ = self.chain.set_head(number - 1);
        })?;
        let gap_row = gap.as_ref().map(|g| (g.number, gap_row(g)));
        let switch = self.hist.write().push(
            Stored {
                hash,
                header,
                miner,
                signs: p.signs,
                body,
                receipts: p.receipts,
            },
            p.codes,
            p.wide,
            snap,
            gap,
        );
        // What the history keeps of a block beyond its window, in the store's consensus tables. They
        // are written before any checkpoint of this block, so a restart that replays from a checkpoint
        // finds the rows of every block at or below it, and an unwind deletes the rows above its
        // target with the blocks.
        if switch.is_some() || gap_row.is_some() {
            self.chain.with_store_mut(|s| -> Result<(), StoreError> {
                if let Some(w) = &switch {
                    s.put_consensus(T::ConsensusEpoch, w.number, &switch_row(w))?;
                }
                if let Some((n, row)) = &gap_row {
                    s.put_consensus(T::ConsensusSnapshot, *n, row)?;
                }
                Ok(())
            })?;
        }
        self.head.send_replace((number, hash));
        self.publish(std::slice::from_ref(self.hist.read().tip()), false);
        if self.ckpt_every != 0 && number % self.ckpt_every == 0 {
            self.checkpoint_in_background()?;
        }
        if self.minimal {
            self.prune()?;
        }
        Ok(out)
    }

    /// Switches to the branch `blocks` (consecutive, the first one's parent is a canonical block of ours): our
    /// blocks above that parent are unwound (`Chain::set_head`, never below the committed block) and taken off
    /// the history, then the branch is imported. Only a branch that ends above our head is taken: geth keeps its V2 head
    /// against a sibling at or below it and reorganises to a block above it whose parent is not the head
    /// (`keepsV2Head`, `writeBlockAndSetHead`, `core/blockchain.go:2419-2442`). If the branch fails to import, our blocks are imported back. Returns (dropped, added).
    /// The safety record is not touched: it lives outside the chain store (ADR-010 section 5).
    pub fn adopt(
        &self,
        blocks: Vec<(Header, Body)>,
        now: u64,
    ) -> Result<(usize, usize), CoreError> {
        let bad = |m: String| CoreError::Inconsistent(m);
        let (Some((first, _)), Some((last, _))) = (blocks.first(), blocks.last()) else {
            return Ok((0, 0));
        };
        let fork = first
            .number
            .checked_sub(1)
            .ok_or_else(|| bad("a branch from genesis".into()))?;
        let (head, _) = self.head();
        if last.number <= head {
            return Err(bad(format!(
                "the branch ends at {}, not above our head {head}",
                last.number
            )));
        }
        if self.hist.read().hash_at(fork) != Some(first.parent_hash) {
            return Err(bad(format!(
                "the branch does not start on our block {fork}"
            )));
        }
        let dropped = self.unwind_to(fork)?;
        let mut added = 0;
        for (h, b) in blocks {
            match self.import(h, b, now) {
                Ok(Outcome::Extended) => added += 1,
                r => {
                    // our own blocks back, the branch is refused
                    let back = self.chain.set_head(fork).is_ok()
                        && self
                            .hist
                            .write()
                            .pop_to(fork)
                            .map(|b| self.publish(&b, true))
                            .is_ok();
                    for s in dropped.iter().filter(|_| back) {
                        if !matches!(
                            self.import(s.header.clone(), s.body.clone(), now),
                            Ok(Outcome::Extended)
                        ) {
                            break;
                        }
                    }
                    return Err(bad(format!("the branch does not import: {r:?}")));
                }
            }
        }
        Ok((dropped.len(), added))
    }

    /// Takes the blocks above `number` off the store (`Chain::set_head`) and the history, never below the
    /// committed block, and only when the history can follow (`HistInner::can_pop_to`). Returns them.
    pub fn unwind_to(&self, number: u64) -> Result<Vec<Stored>, CoreError> {
        let bad = |m: String| CoreError::Inconsistent(m);
        if let Some(c) = self.chain.committed().filter(|c| number < c.number) {
            return Err(bad(format!(
                "block {number} is below the committed block {}",
                c.number
            )));
        }
        // nothing is unwound unless the history can follow the store back
        self.hist.read().can_pop_to(number).map_err(bad)?;
        self.chain
            .set_head(number)
            .map_err(|e| bad(format!("set head {number}: {e}")))?;
        let dropped = self.hist.write().pop_to(number).map_err(bad)?;
        *self.hist.pending() = None;
        self.publish(&dropped, true);
        let hash = self.hist.read().tip().hash;
        self.head.send_replace((number, hash));
        Ok(dropped)
    }

    /// Sends `blocks` (ascending) to the subscription hubs: a canonical block's header and logs, or
    /// with `removed` the logs of blocks a reorg took off (geth `eth/filters/filter_system.go:453`,
    /// `core/blockchain.go:3572`). A block whose receipts do not decode is logged and skipped: the
    /// subscribers miss it, the chain does not.
    fn publish(&self, blocks: &[Stored], removed: bool) {
        let hubs = self.hubs.lock().unwrap_or_else(PoisonError::into_inner);
        for hub in hubs.iter().filter(|h| !h.is_empty()) {
            for s in blocks {
                if let Err(e) = hub.publish_block(
                    &s.header,
                    s.hash,
                    &s.body.transactions,
                    &s.receipts,
                    removed,
                ) {
                    eprintln!(
                        "rpc: subscriptions: block {}: {}",
                        s.header.number, e.message
                    );
                }
            }
        }
    }

    /// Writes a state checkpoint of the head. A snapshot with votes in flight is not saved: the
    /// checkpoint would lose them (the harness's rule). Returns the number saved.
    pub fn checkpoint(&self) -> Result<Option<u64>, CoreError> {
        let Some((c, keep_from)) = self.head_ckpt()? else {
            return Ok(None);
        };
        self.join_save()?;
        save(&self.dir, &c, keep_from)?;
        Ok(Some(c.number))
    }

    /// [`Self::checkpoint`] with the write (gzip JSON, sync, rename) on a thread of its own: the
    /// import takes the state and goes on (geth writes its state outside `insertChain` too,
    /// `core/blockchain.go:2335-2370`). The next checkpoint and the close wait for it; its error is
    /// that of the next checkpoint.
    fn checkpoint_in_background(&self) -> Result<Option<u64>, CoreError> {
        let Some((c, keep_from)) = self.head_ckpt()? else {
            return Ok(None);
        };
        self.join_save()?;
        let (dir, n) = (self.dir.clone(), c.number);
        *self.saving() = Some(std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            save(&dir, &c, keep_from)?;
            eprintln!(
                "chain: state checkpoint of block {n} written in {} ms",
                t0.elapsed().as_millis()
            );
            Ok(())
        }));
        Ok(Some(n))
    }

    fn saving(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<std::thread::JoinHandle<Result<(), FileError>>>> {
        self.saving.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the checkpoint being written, if any.
    fn join_save(&self) -> Result<(), CoreError> {
        match self.saving().take().map(std::thread::JoinHandle::join) {
            None | Some(Ok(Ok(()))) => Ok(()),
            Some(Ok(Err(e))) => Err(e.into()),
            Some(Err(_)) => Err(CoreError::Inconsistent(
                "the state checkpoint writer panicked".into(),
            )),
        }
    }

    /// The checkpoint of the head and the lowest checkpoint number to keep once it is written.
    fn head_ckpt(&self) -> Result<Option<(Ckpt, u64)>, CoreError> {
        let h = self.hist.read();
        let n = h.tip_number();
        let snap = h.snaps.get(&n);
        if n == 0 || snap.is_some_and(|s| !s.votes.is_empty() || !s.tally.is_empty()) {
            return Ok(None);
        }
        let hash = h.tip().hash;
        let mut c = self
            .chain
            .with_store(|s| {
                (s.head() == (n, hash)).then(|| {
                    if s.disk_state() {
                        Ckpt::without_state(n, hash)
                    } else {
                        Ckpt::of_flat(n, hash, s.head_flat())
                    }
                })
            })
            .ok_or_else(|| {
                CoreError::Inconsistent("the store head is not the history tip".into())
            })?;
        c.codes = h.codes.clone();
        if let Some(s) = snap {
            c.signers = s.get_signers();
            c.recents = s.recents.clone();
        }
        c.signs = h
            .blocks
            .iter()
            .filter(|b| !b.signs.is_empty())
            .map(|b| {
                (
                    b.header.number,
                    b.signs.iter().map(|s| (s.sender, s.signed, s.ok)).collect(),
                )
            })
            .collect();
        c.signs_from = h.base;
        c.td = h.td;
        c.gaps = h
            .gaps
            .iter()
            .map(|(n, g)| (*n, g.candidates.clone()))
            .collect();
        drop(h);
        // A restart replays from the newest checkpoint at or below the head and within the unwind
        // window, so an older one is never read: a minimal node keeps the newest only, a full node
        // also the one before it (the newest may be above a head that a crash took back).
        let keep_from = if self.minimal {
            n
        } else {
            n.saturating_sub(self.ckpt_every)
        };
        Ok(Some((c, keep_from)))
    }

    pub fn head(&self) -> (u64, B256) {
        self.chain.head()
    }

    /// Body and receipts of canonical block `n` (`hash`): from the history inside its memory window,
    /// else from the store (`MEMORY_WINDOW`). `None` for a block the node does not hold.
    pub fn records(&self, n: u64, hash: B256) -> Option<(Body, Vec<Bytes>)> {
        Some((self.body(n, hash)?, self.receipts(n, hash)?))
    }

    /// The body of canonical block `n` (`hash`), see [`Self::records`].
    pub fn body(&self, n: u64, hash: B256) -> Option<Body> {
        self.part(n, hash, |s| s.body.clone(), |r| r.body(n, hash))
    }

    /// The receipts of canonical block `n` (`hash`), see [`Self::records`].
    pub fn receipts(&self, n: u64, hash: B256) -> Option<Vec<Bytes>> {
        self.part(n, hash, |s| s.receipts.clone(), |r| r.receipts(n, hash))
    }

    /// `mem` of block `n` inside the memory window, else `disk` of the store. Below the window the
    /// read is the segment file and one MDBX read transaction, with no store lock and no import lock,
    /// so a slow request for old blocks never waits for an import (XB-36). A block the store does not
    /// hold (below a minimal node's retention floor) is `None` at once.
    fn part<T>(
        &self,
        n: u64,
        hash: B256,
        mem: impl FnOnce(&super::hist::Stored) -> T,
        disk: impl FnOnce(&super::hist::Src) -> Result<Option<T>, xone_sync::store::StoreError>,
    ) -> Option<T> {
        {
            let h = self.hist.read();
            match h.at(n) {
                Some(s) if s.hash != hash => return None,
                Some(s) if h.in_memory(n) => return Some(mem(s)),
                None if n > h.tip_number() => return None,
                _ => {}
            }
        }
        disk(&self.reader).ok()?
    }

    /// The number of canonical block `hash`, for a request that names blocks by hash.
    pub fn number_of(&self, hash: B256) -> Option<u64> {
        self.hist.read().number_of(hash)
    }

    /// What a minimal node holds, for the RPC; `None` on a node that followed from genesis.
    pub fn held(&self) -> Option<Held> {
        self.minimal
            .then(|| {
                self.chain
                    .with_store(|s| Held::of_store(&self.spec, s, RETENTION))
                    .ok()
            })
            .flatten()
    }

    /// Deletes what the retention floor of the head has passed, from the store and the history
    /// (`minimal::prune`; the committed block is a ceiling).
    fn prune(&self) -> Result<(), CoreError> {
        let committed = self.chain.committed().map(|l| l.number);
        // a restart goes back to the newest state checkpoint and needs the history of that block
        let every = self.ckpt_every;
        let kept = self
            .chain
            .with_store_mut(|s| {
                let head = s.head().0;
                let restart = (every != 0).then(|| head - head % every);
                minimal::prune_with(&self.spec, s, committed, restart, self.prune_slack)
            })
            .map_err(|e| CoreError::Minimal(e.to_string()))?;
        if let Some(k) = kept {
            let (head, _) = self.chain.head();
            let gaps_from = Retention::derive(&self.spec, head)
                .map_err(|e| CoreError::Minimal(e.to_string()))?
                .floor(Class::GapSnapshot);
            self.hist
                .write()
                .prune(k.headers_from, k.records_from, gaps_from);
        }
        Ok(())
    }
}
