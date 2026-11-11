//! The chain as the node runs it: `xone_sync::chain::Chain` over the [`FileStore`], the executor
//! adapter (`xone_executor::execute_block`) and the header check adapter (`HeaderVerifier`), both
//! reading the node's own [`History`]. A restart re-opens the files and replays every block above
//! the newest state checkpoint through the same `Chain::insert`, so nothing that was stored is
//! trusted without being verified and executed again.

use std::{
    collections::BTreeMap,
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
use xone_executor::{execute_block, ExecCtx};
use xone_primitives::{Header, WideBalance};
use xone_state::{
    trie::flat_root, Account, FlatRead, FlatWrite, MemFlat, Missing, StateDiff, StateRead,
};
use xone_sync::{
    bootstrap::{
        minimal::{self, Held},
        Class, Retention,
    },
    chain::{insert::Outcome, resume::ResumeError, Chain, ChainRead, HeaderCheck, InsertError},
    executor::{BlockExecutor, ExecError, Execution},
    store::{Block, Install, Record, StateRows, Store},
    testkit::MemStore,
};

use super::{
    hist::{reseed, HistInner, Stored},
    store::{self, Ckpt, FileError, FileStore, Loaded},
};

/// Minimal mode deletes when the floor has moved this many blocks past the first held block: a
/// rewrite of the segment files every 256 blocks, not every block.
pub const PRUNE_SLACK: u64 = 256;

/// Blocks above the anchor whose state can be opened (RPC reads at a block, unwind window).
pub const RETENTION: u64 = 1024;
/// Blocks per segment chunk of the in-memory store model.
const BLOCKS_PER_FILE: u64 = 100_000;

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

/// `BlockExecutor` over `execute_block`: the block must extend the tip of the node's history.
pub struct NodeExec {
    spec: ChainSpec,
    hist: std::sync::Arc<History>,
}

impl BlockExecutor for NodeExec {
    fn execute(
        &self,
        parent: &dyn StateRead,
        header: &Header,
        body: &Body,
    ) -> Result<Execution, ExecError> {
        let fail = |e: String| ExecError::Failed(e);
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
        let verify = h
            .context(header, &tip.header, snap)
            .map_err(|e| fail(e.to_string()))?;
        let out = execute_block(
            &self.spec,
            &Dyn(parent),
            header,
            body,
            &ExecCtx {
                parent: &tip.header,
                verify: &verify,
                env: &*h,
                derive: None,
            },
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
            access_list_hash: None,
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

pub type NodeChain = Chain<FileStore, NodeExec, NodeCheck>;

pub struct Core {
    pub spec: ChainSpec,
    pub chain: NodeChain,
    pub hist: std::sync::Arc<History>,
    pub dir: PathBuf,
    pub genesis: B256,
    ckpt_every: u64,
    pub head: watch::Sender<(u64, B256)>,
    /// Started from an imported snapshot: prunes below the retention floor, serves nothing below it.
    pub minimal: bool,
}

/// The blocks of the files up to the checkpoint `start`, with the checkpoint's state, as the store's
/// base: an anchored datadir's snapshot, or a full node's whole chain with its genesis block
/// (`genesis`). The store then holds the window that an export reads.
fn install_of(l: &Loaded, start: u64, flat: &MemFlat, genesis: Option<&Header>) -> Install {
    let take = usize::try_from(start + 1 - l.first).unwrap_or(0);
    let window = || l.blocks.iter().take(take);
    let rec = |h: &Header, body: Body, receipts: Vec<Bytes>| Record {
        block: Block::new(h.clone(), body),
        receipts,
        diff: Default::default(),
        aux: Default::default(),
    };
    Install {
        anchor: l.anchor.clone().unwrap_or_default(),
        headers: genesis
            .into_iter()
            .cloned()
            .chain(window().map(|b| b.header.clone()))
            .collect(),
        records: genesis
            .map(|g| rec(g, Body::default(), vec![]))
            .into_iter()
            .chain(
                window()
                    .filter(|b| b.header.number >= l.records_from)
                    .map(|b| rec(&b.header, b.body.clone(), b.receipts.clone())),
            )
            .collect(),
        state: StateRows {
            accounts: flat.accounts().collect(),
            storage: flat
                .accounts()
                .flat_map(|(a, _)| flat.slots(a).map(move |(k, v)| (a, k, v)))
                .collect(),
        },
        retention: RETENTION,
    }
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
    let header = g.to_header(spec).map_err(e)?;
    if flat_root(&flat) != header.state_root {
        return Err(CoreError::Genesis(
            "the genesis alloc does not give the genesis state root".into(),
        ));
    }
    Ok((flat, codes, header))
}

fn miner_of(spec: &ChainSpec, h: &Header) -> Result<Address, CoreError> {
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

impl Core {
    /// Opens `<dir>`: genesis on a new datadir, else the checkpoint and the stored blocks above it,
    /// replayed and verified. Refuses a datadir whose files do not fit `genesis`.
    pub fn open(
        spec: ChainSpec,
        genesis: &Genesis,
        dir: &Path,
        ckpt_every: u64,
    ) -> Result<Self, CoreError> {
        Self::open_with(spec, genesis, dir, ckpt_every, false)
    }

    /// [`Core::open`], and with `minimal` the start of a node from the anchor of an imported snapshot
    /// (`spec/bootstrap/minimal-mode.md`): the datadir must be anchored, holds nothing below its
    /// first retained block, and prunes to the derived retention floor as the head moves. An
    /// anchored datadir is never opened as a full node, and a full datadir never as a minimal one.
    pub fn open_with(
        spec: ChainSpec,
        genesis: &Genesis,
        dir: &Path,
        ckpt_every: u64,
        minimal: bool,
    ) -> Result<Self, CoreError> {
        let (flat0, codes0, g) = genesis_state(genesis, &spec)?;
        std::fs::create_dir_all(dir).map_err(|e| FileError(format!("{}: {e}", dir.display())))?;
        let loaded = store::load(dir)?;
        let anchored = loaded.as_ref().is_some_and(|l| l.anchor.is_some());
        match (anchored, minimal) {
            (true, false) => return Err(CoreError::Anchored),
            (false, true) => return Err(CoreError::NotAnchored),
            _ => {}
        }
        if let Some(l) = loaded.as_ref().filter(|_| !anchored) {
            if l.blocks.first().map(|b| b.header.parent_hash) != Some(g.hash()) {
                return Err(CoreError::Inconsistent(
                    "the stored chain does not start at this genesis".into(),
                ));
            }
        }
        let ck = loaded.as_ref().and_then(|l| {
            store::load_ckpt(dir, l.number, |c| {
                c.number
                    .checked_sub(l.first)
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|n| l.blocks.get(n))
                    .map(|b| b.header.hash())
                    == Some(c.hash)
            })
        });
        let hist_of = |blocks: Vec<Stored>, codes, snaps, gaps| HistInner {
            spec: spec.clone(),
            base: 0,
            blocks,
            codes,
            wide: BTreeMap::new(),
            snaps,
            gaps,
            switches: vec![],
            tx_index: Default::default(),
            td: U256::ZERO,
        };
        let genesis_block = Stored {
            hash: g.hash(),
            header: g.clone(),
            miner: Address::ZERO,
            signs: vec![],
            body: Body::default(),
            receipts: vec![],
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
                let mut inner = hist_of(
                    vec![],
                    c.codes.clone(),
                    BTreeMap::from([(c.number, snap)]),
                    gaps,
                );
                if anchored {
                    // The history starts at the first retained header. Its total difficulty is not
                    // known (no block below it is held): the window's own sum is a lower bound, and
                    // XDPoS peers rank by block number and round, not by difficulty.
                    inner.base = l.first;
                } else {
                    inner.td = genesis_block.header.difficulty;
                    inner.blocks.push(genesis_block);
                }
                for (i, b) in l.blocks.iter().take((c.number + 1 - l.first) as usize).enumerate() {
                    let n = l.first + i as u64;
                    let s = Stored {
                        hash: b.header.hash(),
                        header: b.header.clone(),
                        miner: miner_of(&spec, &b.header)?,
                        signs: c.signs.get(&n).map_or(vec![], |v| {
                            v.iter()
                                .map(|(sender, signed, ok)| SignTx {
                                    sender: *sender,
                                    signed: *signed,
                                    ok: *ok,
                                })
                                .collect()
                        }),
                        body: b.body.clone(),
                        receipts: b.receipts.clone(),
                    };
                    inner.index(&s, n);
                    inner.td += s.header.difficulty;
                    inner.blocks.push(s);
                }
                let flat = c.flat();
                (c.number, flat, inner)
            }
            _ => {
                let snap = Snapshot::new(
                    0,
                    g.hash(),
                    masternodes_of(&g.extra_data).map_err(|e| CoreError::Genesis(e.to_string()))?,
                );
                let mut inner = hist_of(
                    vec![],
                    codes0,
                    BTreeMap::from([(0, snap)]),
                    Default::default(),
                );
                inner.td = genesis_block.header.difficulty;
                inner.blocks.push(genesis_block);
                (0, flat0, inner)
            }
        };
        inner.wide = flat
            .accounts()
            .filter(|(_, a)| a.balance.is_wide())
            .map(|(a, x)| (a, x.balance))
            .collect();
        let anchor = inner.tip().header.clone();
        if flat_root(&flat) != anchor.state_root {
            return Err(CoreError::Inconsistent(format!(
                "the state checkpoint of block {start} does not give its state root"
            )));
        }
        if anchored && ck.is_none() {
            return Err(CoreError::Inconsistent(
                "an anchored datadir without a state checkpoint at or below its head".into(),
            ));
        }
        // With a checkpoint the in-memory store is the blocks of the files up to it, installed like a
        // snapshot: its manifest says what the files hold (`first_block`), and an export, the
        // retention check and the pruning see the same window the history does.
        let install = match (&loaded, &ck) {
            (Some(l), Some(_)) => Some(install_of(l, start, &flat, (!anchored).then_some(&g))),
            _ => None,
        };
        let hist = std::sync::Arc::new(History {
            inner: RwLock::new(inner),
            pending: Mutex::new(None),
        });
        let mem = match install {
            Some(i) => {
                let mut m = MemStore::empty(BLOCKS_PER_FILE);
                m.install_snapshot(i)
                    .map_err(|e| CoreError::Inconsistent(e.to_string()))?;
                m
            }
            None => MemStore::with_anchor(anchor, flat, RETENTION, BLOCKS_PER_FILE),
        };
        let files = FileStore::new(mem, dir, loaded.as_ref())?;
        let exec = NodeExec {
            spec: spec.clone(),
            hist: hist.clone(),
        };
        let check = NodeCheck {
            spec: spec.clone(),
            hist: hist.clone(),
        };
        let (chain, _) = Chain::open(spec.clone(), files, exec, check)?;
        let (head, _) = watch::channel((start, hist.read().tip().hash));
        let core = Self {
            spec,
            chain,
            hist,
            dir: dir.to_owned(),
            genesis: g.hash(),
            ckpt_every,
            head,
            minimal,
        };
        if minimal {
            core.check_start()?;
        }
        if let Some(l) = loaded {
            for b in l.blocks.into_iter().skip((start + 1 - l.first) as usize) {
                let number = b.header.number;
                match core.import(b.header, b.body, now())? {
                    Outcome::Extended => {}
                    o => {
                        return Err(CoreError::Inconsistent(format!(
                            "stored block {number} replayed as {o:?}"
                        )))
                    }
                }
            }
            if core.chain.head() != (l.number, l.hash) {
                return Err(CoreError::Inconsistent(format!(
                    "the replay ends at {:?}, head.json says {} {}",
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
        let p = self
            .hist
            .pending()
            .take()
            .filter(|p| p.hash == hash)
            .ok_or_else(|| CoreError::Inconsistent(format!("no execution record for {hash}")))?;
        let (mut snap, mut gap, mut err) = (p.snap, None, None);
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
        self.hist.write().push(
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
        self.head.send_replace((number, hash));
        if self.ckpt_every != 0 && number % self.ckpt_every == 0 {
            self.checkpoint()?;
        }
        if self.minimal {
            self.prune()?;
        }
        Ok(out)
    }

    /// Writes a state checkpoint of the head. A snapshot with votes in flight is not saved: the
    /// checkpoint would lose them (the harness's rule). Returns the number saved.
    pub fn checkpoint(&self) -> Result<Option<u64>, CoreError> {
        let h = self.hist.read();
        let n = h.tip_number();
        let snap = h.snaps.get(&n);
        if n == 0 || snap.is_some_and(|s| !s.votes.is_empty() || !s.tally.is_empty()) {
            return Ok(None);
        }
        let hash = h.tip().hash;
        let mut c = self
            .chain
            .with_store(|s| (s.head() == (n, hash)).then(|| Ckpt::of_flat(n, hash, s.head_flat())))
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
        c.gaps = h
            .gaps
            .iter()
            .map(|(n, g)| (*n, g.candidates.clone()))
            .collect();
        store::save_ckpt(&self.dir, &c)?;
        if self.minimal {
            store::drop_ckpts_below(&self.dir, n);
        }
        Ok(Some(n))
    }

    pub fn head(&self) -> (u64, B256) {
        self.chain.head()
    }

    fn check_start(&self) -> Result<(), CoreError> {
        self.chain
            .with_store(|s| minimal::check_start(&self.spec, s))
            .map_err(|e| CoreError::Minimal(e.to_string()))
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
        let kept = self
            .chain
            .with_store_mut(|s| minimal::prune(&self.spec, s, committed, PRUNE_SLACK))
            .map_err(|e| CoreError::Minimal(e.to_string()))?;
        if let Some(k) = kept {
            let (head, _) = self.chain.head();
            let gaps_from = Retention::derive(&self.spec, head)
                .map_err(|e| CoreError::Minimal(e.to_string()))?
                .floor(Class::GapSnapshot);
            self.hist.write().prune(k.headers_from, k.records_from, gaps_from);
        }
        Ok(())
    }
}
