//! The snap/1 client: state download at a root with every range proven, the heal pass that
//! repairs what a moved pivot left stale, and the completeness check against what the first
//! post-pivot block needs (`spec/snap/completeness.md`).
//!
//! Sans-IO: all traffic goes through [`PeerSource::exchange`], one request and one answer. The
//! driver never accepts a range without its proof ([`crate::proof::verify_range`]), never writes
//! data it has not verified, and keeps everything it needs to resume in the [`StateStore`]: a run
//! that dies at any point continues from the store, and a range that was stored is not asked for
//! again.
//!
//! ponytail: the store is memory and keyed by hashed address and slot (a snap response carries no
//! preimages, so the address-keyed `PlainAccountState` of ADR-005 cannot be filled from it).
//! Persistence and the hashed tables are the MDBX binding's job; the driver only needs a store
//! that survives the process.

use alloy_primitives::{keccak256, Bytes, B256, KECCAK256_EMPTY, U256};
use alloy_rlp::Decodable;
use alloy_trie::{Nibbles, EMPTY_ROOT_HASH};
use std::collections::{BTreeMap, HashMap, HashSet};
use xone_primitives::WideBalanceError;
use xone_state::Account;

use crate::{
    codec::{
        GetAccountRange, GetByteCodes, GetStorageRanges, GetTrieNodes, Message, MAX_PROOF_NODES,
    },
    mpt::{self, compact, NodeError, Trie},
    proof::{verify_range, ProofError},
    server::{hashed_root, Snapshot},
};

/// Why one exchange failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeerError {
    #[error("timeout")]
    Timeout,
    #[error("disconnected")]
    Disconnected,
    /// The answer was empty: the peer does not have the state at the requested root (pruned, or
    /// not synced that far). geth marks such a peer stateless (`sync.go:2565`).
    #[error("the peer has no data for the request")]
    Unavailable,
}

/// A bad answer. Reported to the source (which may score or drop the peer) and retried.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Fault {
    #[error("peer: {0}")]
    Peer(#[from] PeerError),
    #[error("proof: {0}")]
    Proof(#[from] ProofError),
    #[error("bad response: {0}")]
    Bad(&'static str),
    /// A proven leaf above the legal balance (a fatal error, not a retry).
    #[error("account {0} has an illegal balance: {1}")]
    Balance(B256, WideBalanceError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// Every attempt got an empty answer: replace the pivot ([`StateStore::set_root`]) and run
    /// again.
    #[error("no peer can serve the state at the pivot root; replace the pivot")]
    Unavailable,
    #[error("peer failed: {0}")]
    Peer(PeerError),
    #[error("{0}")]
    Fault(Fault),
    #[error("trie: {0}")]
    Node(#[from] NodeError),
    #[error("state root {got} after the download, expected {want}")]
    Root { got: B256, want: B256 },
    #[error("sync is not complete: {0}")]
    Incomplete(Incomplete),
}

/// One request and its answer. The source picks the peer, frames the message (the RLPx offset of
/// snap/1 is 16 plus the eth length), applies the timeout, and decodes the answer.
pub trait PeerSource {
    fn exchange(&mut self, req: Message) -> Result<Message, PeerError>;
    /// A bad answer, for scoring. The default ignores it.
    fn report(&mut self, _fault: &Fault) {}
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// `bytes` of account and storage requests (geth `maxRequestSize`, `sync.go:56`).
    pub request_bytes: u64,
    /// Accounts in one storage request (geth: `cap / 1024`, `sync.go:1328`).
    pub storage_accounts: usize,
    /// Hashes in one bytecode request (geth `maxCodeRequestCount`, `sync.go:66`).
    pub codes: usize,
    /// Paths in one heal request (geth `maxTrieRequestCount`, `sync.go:72`).
    pub nodes: usize,
    /// Consecutive failures of one request after which the run stops.
    pub max_failures: u32,
    pub max_heal_rounds: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            request_bytes: 512 * 1024,
            storage_accounts: 128,
            codes: 84,
            nodes: 1024,
            max_failures: 8,
            max_heal_rounds: 128,
        }
    }
}

/// Where a run stands. Everything else is derived from the data in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The state root being downloaded.
    pub root: B256,
    /// First account hash not yet downloaded; `None` when the account range is done.
    pub next_account: Option<B256>,
    /// Per account with a storage trie: the next slot hash to ask for, `None` when done. An
    /// account that is absent has not started.
    pub storage_next: BTreeMap<B256, Option<B256>>,
    /// The root the whole download was checked against, once it was.
    pub verified: Option<B256>,
    /// The pivot moved after data was downloaded: the accounts must be reconciled with the new
    /// root before storage is fetched for them.
    pub stale: bool,
}

/// The downloaded state, by hashed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateStore {
    pub accounts: BTreeMap<B256, Account>,
    /// Slot hash to value, per account hash.
    pub storage: BTreeMap<B256, BTreeMap<B256, U256>>,
    pub codes: BTreeMap<B256, Bytes>,
    pub progress: Progress,
}

impl StateStore {
    pub fn new(root: B256) -> Self {
        Self {
            accounts: BTreeMap::new(),
            storage: BTreeMap::new(),
            codes: BTreeMap::new(),
            progress: Progress {
                root,
                next_account: (root != EMPTY_ROOT_HASH).then_some(B256::ZERO),
                storage_next: BTreeMap::new(),
                verified: None,
                stale: false,
            },
        }
    }

    /// Replaces the pivot. What was downloaded stays: the cursors go on, and the heal pass of the
    /// next run repairs whatever the new root changed (geth does the same, `sync.go:622-700`).
    pub fn set_root(&mut self, root: B256) {
        self.progress.root = root;
        self.progress.verified = None;
        self.progress.stale = !self.accounts.is_empty();
        if root == EMPTY_ROOT_HASH {
            *self = Self::new(root);
        }
    }

    /// The state in a form the [server](crate::server) answers from.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::from_hashed(self.accounts.clone(), &self.storage, self.codes.clone())
    }
}

/// The result of one walk of the tries.
struct Walked {
    accounts: BTreeMap<B256, Account>,
    storage: BTreeMap<B256, BTreeMap<B256, U256>>,
    /// The account whose storage trie the node belongs to (`None`: the account trie), its path
    /// and its hash.
    missing: Vec<(Option<B256>, Nibbles, B256)>,
}

struct Syncer<'a, P> {
    store: &'a mut StateStore,
    src: &'a mut P,
    cfg: &'a Config,
    id: u64,
    /// Nodes fetched by the heal pass, by hash.
    healed: HashMap<B256, Bytes>,
}

/// Downloads the state at `store.progress.root` from `src`, heals it, and checks the root.
/// Returns once `store.progress.verified` is the root. Run it again after any error: it
/// continues from the store.
pub fn sync<P: PeerSource>(
    store: &mut StateStore,
    src: &mut P,
    cfg: &Config,
) -> Result<(), SyncError> {
    let mut s = Syncer {
        store,
        src,
        cfg,
        id: 0,
        healed: HashMap::new(),
    };
    s.accounts()?;
    s.reconcile()?;
    s.storage()?;
    s.codes()?;
    s.heal()?;
    s.codes()?;
    s.verify()
}

/// Slot hash to value.
pub type Slots = BTreeMap<B256, U256>;

/// One account and its whole storage at `root`, every range proven: the account by the range
/// proof of its single key (an absent account is `Ok(None)`, proven absent), then its slots
/// against its storage root, and the root of the slots computed again at the end. Used for the
/// 0x88 state at a gap block (`spec/snap/pivot.md`), where only one contract is needed.
pub fn account_at<P: PeerSource>(
    src: &mut P,
    cfg: &Config,
    root: B256,
    hash: B256,
) -> Result<Option<(Account, Slots)>, SyncError> {
    let mut store = StateStore::new(root);
    store.progress.next_account = None;
    let mut s = Syncer {
        store: &mut store,
        src,
        cfg,
        id: 0,
        healed: HashMap::new(),
    };
    let mut found = None;
    s.retry(|s| {
        found = s.one_account(hash)?;
        Ok(())
    })?;
    let Some(acc) = found else { return Ok(None) };
    s.store.accounts.insert(hash, acc.clone());
    s.storage()?;
    // no entry: the account has no storage; the root check below holds either way
    let slots: Slots = store.storage.remove(&hash).into_iter().flatten().collect();
    let got =
        xone_state::trie::root_of_hashed(slots.iter().map(|(k, v)| (*k, alloy_rlp::encode(v))));
    if got != acc.storage_root {
        return Err(SyncError::Root {
            got,
            want: acc.storage_root,
        });
    }
    Ok(Some((acc, slots)))
}

fn inc(h: B256) -> Option<B256> {
    U256::from_be_bytes(h.0)
        .checked_add(U256::from(1u8))
        .map(|v| B256::from(v.to_be_bytes::<32>()))
}

fn slot_value(body: &[u8]) -> Result<U256, Fault> {
    let mut b = body;
    let v = U256::decode(&mut b).map_err(|_| Fault::Bad("slot value is not an integer"))?;
    if !b.is_empty() || v.is_zero() || alloy_rlp::encode(v) != body {
        return Err(Fault::Bad("slot value is not canonical or is zero"));
    }
    Ok(v)
}

fn full_account(leaf: &[u8]) -> Result<Account, Fault> {
    let mut b = leaf;
    let a = Account::decode(&mut b).map_err(|_| Fault::Bad("account leaf"))?;
    if !b.is_empty() {
        return Err(Fault::Bad("account leaf has trailing bytes"));
    }
    Ok(a)
}

impl<P: PeerSource> Syncer<'_, P> {
    fn ask(&mut self, mk: impl FnOnce(u64) -> Message) -> Result<Message, Fault> {
        self.id += 1;
        let req = mk(self.id);
        let reply = self.src.exchange(req)?;
        if reply.id() == self.id {
            Ok(reply)
        } else {
            Err(Fault::Bad("response id does not match the request"))
        }
    }

    /// Runs `step` until it succeeds or fails `max_failures` times in a row.
    fn retry(
        &mut self,
        mut step: impl FnMut(&mut Self) -> Result<(), Fault>,
    ) -> Result<(), SyncError> {
        let mut failures = 0;
        loop {
            let fault = match step(self) {
                Ok(()) => return Ok(()),
                Err(f @ Fault::Balance(..)) => return Err(SyncError::Fault(f)),
                Err(f) => f,
            };
            self.src.report(&fault);
            failures += 1;
            if failures >= self.cfg.max_failures {
                return Err(match fault {
                    Fault::Peer(PeerError::Unavailable) => SyncError::Unavailable,
                    Fault::Peer(e) => SyncError::Peer(e),
                    f => SyncError::Fault(f),
                });
            }
        }
    }

    // ---- accounts

    fn accounts(&mut self) -> Result<(), SyncError> {
        while let Some(origin) = self.store.progress.next_account {
            self.retry(|s| s.account_step(origin))?;
        }
        Ok(())
    }

    /// `OnAccounts`, `sync.go:2516-2620` (the proof check at 2586-2589).
    fn account_step(&mut self, origin: B256) -> Result<(), Fault> {
        let (root, bytes) = (self.store.progress.root, self.cfg.request_bytes);
        let Message::AccountRange(r) = self.ask(|id| {
            Message::GetAccountRange(GetAccountRange {
                id,
                root,
                origin,
                limit: B256::repeat_byte(0xff),
                bytes,
            })
        })?
        else {
            return Err(Fault::Bad("not an AccountRange"));
        };
        if r.accounts.is_empty() && r.proof.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        if r.proof.len() > MAX_PROOF_NODES {
            return Err(Fault::Bad("too many proof nodes"));
        }
        let mut accounts = Vec::with_capacity(r.accounts.len());
        for a in &r.accounts {
            let acc = Account::from_slim_rlp(&a.body).map_err(|_| Fault::Bad("slim account"))?;
            accounts.push((a.hash, acc));
        }
        let keys: Vec<B256> = accounts.iter().map(|(h, _)| *h).collect();
        let values: Vec<Bytes> = accounts.iter().map(|(_, a)| a.rlp().into()).collect();
        let more = verify_range(root, origin, &keys, &values, &r.proof)?;
        for (h, a) in &accounts {
            a.balance.check_legal().map_err(|e| Fault::Balance(*h, e))?;
        }
        self.store.accounts.extend(accounts);
        self.store.progress.next_account = keys.last().filter(|_| more).and_then(|k| inc(*k));
        Ok(())
    }

    /// The account at `hash` alone: a range from `hash` to `hash`, proven.
    fn one_account(&mut self, hash: B256) -> Result<Option<Account>, Fault> {
        let (root, bytes) = (self.store.progress.root, self.cfg.request_bytes);
        let Message::AccountRange(r) = self.ask(|id| {
            Message::GetAccountRange(GetAccountRange {
                id,
                root,
                origin: hash,
                limit: hash,
                bytes,
            })
        })?
        else {
            return Err(Fault::Bad("not an AccountRange"));
        };
        if r.accounts.is_empty() && r.proof.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        if r.proof.len() > MAX_PROOF_NODES {
            return Err(Fault::Bad("too many proof nodes"));
        }
        let mut accounts = Vec::with_capacity(r.accounts.len());
        for a in &r.accounts {
            let acc = Account::from_slim_rlp(&a.body).map_err(|_| Fault::Bad("slim account"))?;
            accounts.push((a.hash, acc));
        }
        let keys: Vec<B256> = accounts.iter().map(|(h, _)| *h).collect();
        let values: Vec<Bytes> = accounts.iter().map(|(_, a)| a.rlp().into()).collect();
        verify_range(root, hash, &keys, &values, &r.proof)?;
        Ok(accounts
            .into_iter()
            .next()
            .filter(|(h, _)| *h == hash)
            .map(|(_, a)| a))
    }

    // ---- storage

    /// The accounts whose storage is still to download: one that is partly done (alone, with its
    /// origin), or up to `storage_accounts` that have not started.
    fn storage_batch(&self) -> (B256, Vec<(B256, B256)>) {
        let mut batch = Vec::new();
        for (h, a) in &self.store.accounts {
            if a.storage_root == EMPTY_ROOT_HASH {
                continue;
            }
            match self.store.progress.storage_next.get(h) {
                Some(None) => {}
                Some(Some(o)) if batch.is_empty() => return (*o, vec![(*h, a.storage_root)]),
                Some(Some(_)) => {}
                None => {
                    batch.push((*h, a.storage_root));
                    if batch.len() >= self.cfg.storage_accounts {
                        break;
                    }
                }
            }
        }
        (B256::ZERO, batch)
    }

    fn storage(&mut self) -> Result<(), SyncError> {
        loop {
            let (origin, batch) = self.storage_batch();
            if batch.is_empty() {
                return Ok(());
            }
            self.retry(|s| s.storage_step(origin, &batch))?;
        }
    }

    /// `OnStorage`, `sync.go:2735-2885`.
    fn storage_step(&mut self, origin: B256, batch: &[(B256, B256)]) -> Result<(), Fault> {
        let (root, bytes) = (self.store.progress.root, self.cfg.request_bytes);
        let late = origin != B256::ZERO;
        let Message::StorageRanges(r) = self.ask(|id| {
            Message::GetStorageRanges(GetStorageRanges {
                id,
                root,
                accounts: batch.iter().map(|(h, _)| *h).collect(),
                origin: if late {
                    origin.0.to_vec().into()
                } else {
                    Bytes::new()
                },
                limit: if late {
                    vec![0xff; 32].into()
                } else {
                    Bytes::new()
                },
                bytes,
            })
        })?
        else {
            return Err(Fault::Bad("not a StorageRanges"));
        };
        if r.slots.is_empty() && r.proof.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        if r.slots.len() > batch.len() {
            return Err(Fault::Bad("more slot sets than accounts"));
        }
        if r.proof.len() > MAX_PROOF_NODES {
            return Err(Fault::Bad("too many proof nodes"));
        }
        // a proof with no slots: the range that was asked for is empty (`sync.go:2822-2830`)
        let empty = vec![vec![]];
        let sets = if r.slots.is_empty() { &empty } else { &r.slots };
        let mut done = Vec::new();
        for (i, set) in sets.iter().enumerate() {
            let (acct, sroot) = batch[i];
            let keys: Vec<B256> = set.iter().map(|s| s.hash).collect();
            let values: Vec<Bytes> = set.iter().map(|s| s.body.clone()).collect();
            let slots = set
                .iter()
                .map(|s| Ok((s.hash, slot_value(&s.body)?)))
                .collect::<Result<Vec<_>, Fault>>()?;
            // only the last set can be partial, and only it has a proof (`handlers.go:257-290`)
            let more = if i + 1 == sets.len() && !r.proof.is_empty() {
                let o = if i == 0 { origin } else { B256::ZERO };
                verify_range(sroot, o, &keys, &values, &r.proof)?
            } else {
                verify_range(sroot, B256::ZERO, &keys, &values, &[])?
            };
            done.push((
                acct,
                slots,
                more.then(|| keys.last().and_then(|k| inc(*k))).flatten(),
            ));
        }
        for (acct, slots, next) in done {
            self.store.storage.entry(acct).or_default().extend(slots);
            self.store.progress.storage_next.insert(acct, next);
        }
        Ok(())
    }

    // ---- code

    fn codes(&mut self) -> Result<(), SyncError> {
        loop {
            let mut need = Vec::new();
            let mut seen = HashSet::new();
            for a in self.store.accounts.values() {
                let h = a.code_hash;
                if h != KECCAK256_EMPTY && !self.store.codes.contains_key(&h) && seen.insert(h) {
                    need.push(h);
                    if need.len() >= self.cfg.codes {
                        break;
                    }
                }
            }
            if need.is_empty() {
                return Ok(());
            }
            self.retry(|s| s.code_step(&need))?;
        }
    }

    /// `onByteCodes`, `sync.go:2635-2735` (matching at 2698): the answer skips what the peer lacks, so each code is
    /// matched to the next requested hash it hashes to.
    fn code_step(&mut self, need: &[B256]) -> Result<(), Fault> {
        let bytes = self.cfg.request_bytes;
        let Message::ByteCodes(r) = self.ask(|id| {
            Message::GetByteCodes(GetByteCodes {
                id,
                hashes: need.to_vec(),
                bytes,
            })
        })?
        else {
            return Err(Fault::Bad("not a ByteCodes"));
        };
        if r.codes.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        let mut j = 0;
        let mut got = Vec::new();
        for c in r.codes {
            let h = keccak256(&c);
            while j < need.len() && need[j] != h {
                j += 1;
            }
            if j == need.len() {
                return Err(Fault::Bad("code that was not requested"));
            }
            got.push((h, c));
            j += 1;
        }
        self.store.codes.extend(got);
        Ok(())
    }

    // ---- heal

    /// One walk of the account trie (and, with `storage`, of the storage tries that differ from
    /// the local slots) from the root, over the nodes that can be built from the local data plus the
    /// nodes fetched so far. `Walked::storage` holds only the tries that were walked: a storage trie
    /// whose root the local slots already hash to is left as it is, and no node of it is built (a
    /// network's state does not fit in memory as nodes, `spec/snap/pivot.md`).
    fn walk_state(&self, storage: bool) -> Result<Walked, SyncError> {
        let mut known: HashMap<B256, Bytes> = self.healed.clone();
        let accounts = self
            .store
            .accounts
            .iter()
            .map(|(h, a)| (*h, a.rlp()))
            .collect();
        known.extend(
            Trie::build(&accounts)
                .nodes_by_hash()
                .map(|(h, n)| (h, n.clone())),
        );
        let w = mpt::walk(self.store.progress.root, &known)?;
        let mut out = Walked {
            missing: w.missing.into_iter().map(|(p, h)| (None, p, h)).collect(),
            accounts: BTreeMap::new(),
            storage: BTreeMap::new(),
        };
        for (k, leaf) in w.leaves {
            let acc = full_account(&leaf).map_err(SyncError::Fault)?;
            acc.balance
                .check_legal()
                .map_err(|e| SyncError::Fault(Fault::Balance(k, e)))?;
            let local = self.store.storage.get(&k);
            let same = |s: &BTreeMap<B256, U256>| {
                xone_state::trie::root_of_hashed(s.iter().map(|(h, v)| (*h, alloy_rlp::encode(v))))
                    == acc.storage_root
            };
            if storage && acc.storage_root != EMPTY_ROOT_HASH && !local.is_some_and(same) {
                let leaves = local
                    .into_iter()
                    .flatten()
                    .map(|(s, v)| (*s, alloy_rlp::encode(v)))
                    .collect();
                let mut nodes = self.healed.clone();
                nodes.extend(
                    Trie::build(&leaves)
                        .nodes_by_hash()
                        .map(|(h, n)| (h, n.clone())),
                );
                let sw = mpt::walk(acc.storage_root, &nodes)?;
                out.missing
                    .extend(sw.missing.into_iter().map(|(p, h)| (Some(k), p, h)));
                let slots = sw
                    .leaves
                    .iter()
                    .map(|(s, v)| Ok((*s, slot_value(v)?)))
                    .collect::<Result<BTreeMap<_, _>, Fault>>()
                    .map_err(SyncError::Fault)?;
                out.storage.insert(k, slots);
            }
            out.accounts.insert(k, acc);
        }
        Ok(out)
    }

    /// Fetches the missing nodes, by path, in requests of `nodes` paths.
    fn fetch_nodes(&mut self, missing: &[(Option<B256>, Nibbles, B256)]) -> Result<(), SyncError> {
        for chunk in missing.chunks(self.cfg.nodes) {
            self.retry(|s| s.node_step(chunk))?;
        }
        Ok(())
    }

    /// After a pivot move: makes the downloaded accounts the accounts of the trie at the new root.
    /// An account that is gone is dropped with its storage; one whose storage root changed starts
    /// its storage over. Without this, a storage request would name an account the new state
    /// does not have, and be answered empty.
    fn reconcile(&mut self) -> Result<(), SyncError> {
        if !self.store.progress.stale {
            return Ok(());
        }
        for _ in 0..self.cfg.max_heal_rounds {
            let w = self.walk_state(false)?;
            if w.missing.is_empty() {
                let (store, new) = (&mut *self.store, w.accounts);
                for (h, a) in &new {
                    if store
                        .accounts
                        .get(h)
                        .is_some_and(|o| o.storage_root != a.storage_root)
                    {
                        store.storage.remove(h);
                        store.progress.storage_next.remove(h);
                    }
                }
                store.storage.retain(|h, _| new.contains_key(h));
                store
                    .progress
                    .storage_next
                    .retain(|h, _| new.contains_key(h));
                store.accounts = new;
                store.progress.stale = false;
                return Ok(());
            }
            self.fetch_nodes(&w.missing)?;
        }
        Err(SyncError::Fault(Fault::Bad("reconcile did not converge")))
    }

    /// Makes the downloaded leaves the leaves of the tries at the root. When the data already hashes
    /// to the root there is nothing to do. Otherwise the tries are walked from the root over the
    /// nodes that can be built from the local data plus the nodes fetched so far; every missing node
    /// is asked for by path (`GetTrieNodes`) and checked by its hash, until the walk is complete and
    /// its leaves replace the local data. Rounds are bounded by the depth of the tries.
    ///
    /// ponytail: each round walks and rebuilds from the whole store; geth keeps a persistent
    /// frontier of paths (`trie/sync.go`). Fetched nodes are not kept across runs.
    fn heal(&mut self) -> Result<(), SyncError> {
        if hashed_root(&self.store.accounts, &self.store.storage) == self.store.progress.root {
            self.finish_accounts();
            return Ok(());
        }
        for _ in 0..self.cfg.max_heal_rounds {
            let w = self.walk_state(true)?;
            if w.missing.is_empty() {
                self.store.accounts = w.accounts;
                self.store.storage.extend(w.storage);
                // an account without storage at the root keeps none
                let accounts = &self.store.accounts;
                self.store.storage.retain(|h, _| {
                    accounts
                        .get(h)
                        .is_some_and(|a| a.storage_root != EMPTY_ROOT_HASH)
                });
                self.finish_accounts();
                return Ok(());
            }
            self.fetch_nodes(&w.missing)?;
        }
        Err(SyncError::Fault(Fault::Bad("heal did not converge")))
    }

    /// Every cursor is done: the leaves are final.
    fn finish_accounts(&mut self) {
        let p = &mut self.store.progress;
        p.next_account = None;
        p.storage_next = self
            .store
            .accounts
            .iter()
            .filter(|(_, a)| a.storage_root != EMPTY_ROOT_HASH)
            .map(|(h, _)| (*h, None))
            .collect();
    }

    /// `OnTrieNodes`, `sync.go:2886-2990` (matching at 2951).
    fn node_step(&mut self, chunk: &[(Option<B256>, Nibbles, B256)]) -> Result<(), Fault> {
        let mut paths: Vec<Vec<Bytes>> = Vec::new();
        let mut last: Option<B256> = None;
        for (acct, path, _) in chunk {
            match (acct, last) {
                (None, _) => paths.push(vec![compact(path)]),
                (Some(a), Some(l)) if *a == l => {
                    if let Some(set) = paths.last_mut() {
                        set.push(compact(path));
                    }
                }
                (Some(a), _) => paths.push(vec![a.0.to_vec().into(), compact(path)]),
            }
            last = *acct;
        }
        let want: Vec<B256> = chunk.iter().map(|(_, _, h)| *h).collect();
        let (root, bytes) = (self.store.progress.root, self.cfg.request_bytes);
        let Message::TrieNodes(r) = self.ask(|id| {
            Message::GetTrieNodes(GetTrieNodes {
                id,
                root,
                paths,
                bytes,
            })
        })?
        else {
            return Err(Fault::Bad("not a TrieNodes"));
        };
        if r.nodes.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        let mut j = 0;
        let mut got = Vec::new();
        for n in r.nodes {
            if n.is_empty() {
                continue; // a gap: the peer has no node for that path
            }
            let h = keccak256(&n);
            while j < want.len() && want[j] != h {
                j += 1;
            }
            if j == want.len() {
                return Err(Fault::Bad("trie node that was not requested"));
            }
            got.push((h, n));
            j += 1;
        }
        if got.is_empty() {
            return Err(PeerError::Unavailable.into());
        }
        self.healed.extend(got);
        Ok(())
    }

    fn verify(&mut self) -> Result<(), SyncError> {
        let want = self.store.progress.root;
        let got = hashed_root(&self.store.accounts, &self.store.storage);
        if got != want {
            return Err(SyncError::Root { got, want });
        }
        self.store.progress.verified = Some(want);
        Ok(())
    }
}

// ---- completeness

/// A kind of data below the pivot that the first post-pivot block needs
/// (`xone_sync::bootstrap::Class`, same variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Header,
    Body,
    Receipt,
    GapSnapshot,
    /// The state of the pivot block: held when the download is verified.
    State,
}

/// An inclusive range of one kind (`xone_sync::bootstrap::Need`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub kind: Kind,
    pub first: u64,
    pub last: u64,
}

/// The pre-pivot data a node holds, and how to get more of it (eth `GetBlockHeaders`,
/// `GetBlockBodies`, `GetReceipts`; geth `PrimeAncestorHeaders` and `PrimeAncestorBodies`,
/// `eth/downloader/downloader.go:658,714`).
pub trait History {
    fn has(&self, kind: Kind, number: u64) -> bool;
    /// Fetches, verifies and stores `first..=last` of `kind`. The caller checks the outcome with
    /// [`History::has`]; an error here is reported, not trusted.
    fn fetch(&mut self, kind: Kind, first: u64, last: u64) -> Result<(), PeerError>;
}

/// What is missing, as inclusive ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incomplete(pub Vec<Need>);

impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, n) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{:?} {}..={}", n.kind, n.first, n.last)?;
        }
        Ok(())
    }
}

fn gaps(needs: &[Need], store: &StateStore, state_root: B256, h: &impl History) -> Vec<Need> {
    let mut out = Vec::new();
    for n in needs {
        if n.kind == Kind::State {
            if store.progress.verified != Some(state_root) {
                out.push(*n);
            }
            continue;
        }
        let mut open: Option<u64> = None;
        for b in n.first..=n.last {
            match (h.has(n.kind, b), open) {
                (false, None) => open = Some(b),
                (true, Some(a)) => {
                    out.push(Need {
                        kind: n.kind,
                        first: a,
                        last: b - 1,
                    });
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(a) = open {
            out.push(Need {
                kind: n.kind,
                first: a,
                last: n.last,
            });
        }
    }
    out
}

/// Calls the sync complete only when everything in `needs` is held: the pivot state verified
/// against `state_root` (the state root of the pivot header) and every other range present. A
/// missing range is fetched first; what is still missing afterwards is [`SyncError::Incomplete`].
pub fn finish(
    store: &StateStore,
    state_root: B256,
    needs: &[Need],
    history: &mut impl History,
) -> Result<(), SyncError> {
    for g in gaps(needs, store, state_root, history) {
        if g.kind != Kind::State {
            // an error here shows up as a gap below
            let _ = history.fetch(g.kind, g.first, g.last);
        }
    }
    let rest = gaps(needs, store, state_root, history);
    if rest.is_empty() {
        Ok(())
    } else {
        Err(SyncError::Incomplete(Incomplete(rest)))
    }
}
