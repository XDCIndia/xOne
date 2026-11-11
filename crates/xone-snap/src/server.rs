//! The snap/1 server: answers the four requests from a [`Snapshot`] of one state, with geth's
//! response limits (`eth/protocols/snap/handler.go:32-54`, `handlers.go:56-537`) and the ADR-007
//! Q15 bounds on top (4 sessions, 1 per peer, 16 MiB/s; a rejected request is answered empty and
//! scored `RateExceeded`, a peer that keeps going is disconnected).
//!
//! No I/O and no clock: `now_ms` is an argument. A node that has no snapshot has no [`Snapshot`],
//! so it has no [`Server`] and must not advertise snap/1 (there is no stub that answers empty).

use alloy_primitives::{keccak256, Bytes, B256, B512, KECCAK256_EMPTY, U256};
use alloy_trie::{HashBuilder, Nibbles, EMPTY_ROOT_HASH};
use std::collections::{BTreeMap, HashMap};
use xone_state::{flat::FlatRead, provider::StateProvider, Account, FlatWrite};

use crate::{
    codec::{
        AccountData, AccountRange, ByteCodes, GetAccountRange, GetByteCodes, GetStorageRanges,
        GetTrieNodes, Message, StorageData, StorageRanges, TrieNodes, MAX_LOOKUPS,
        SOFT_RESPONSE_LIMIT,
    },
    mpt::{from_compact, Trie},
};

/// geth `stateLookupSlack` (`handler.go:41`): a storage response may exceed `bytes` by 10 % so
/// that a contract is not split for a few bytes. Integer form of `uint64(float64(b) * 1.1)`.
fn hard_limit(bytes: u64) -> u64 {
    bytes.saturating_add(bytes / 10)
}

/// One state, ready to serve: the accounts and storage by hashed key, the tries with every node,
/// and the contract code by hash.
///
/// ponytail: built from the whole table, held in memory, one state (the head); the server
/// answers a root only if it is this one (geth serves the last 128 layers of its snapshot tree).
/// A snapshot over the MDBX flat tables with an incremental trie replaces it.
#[derive(Debug, Default)]
pub struct Snapshot {
    root: B256,
    accounts: BTreeMap<B256, Account>,
    account_trie: Trie,
    storage: HashMap<B256, Slots>,
    /// Contract code by code hash; the code table of the node.
    pub codes: BTreeMap<B256, Bytes>,
}

#[derive(Debug)]
struct Slots {
    values: BTreeMap<B256, Bytes>,
    trie: Trie,
}

impl Snapshot {
    /// From hashed state. The `storage_root` of each account is recomputed from its slots, as
    /// `xone_state::trie::flat_root` does; zero slots are dropped.
    pub fn from_hashed(
        accounts: BTreeMap<B256, Account>,
        storage: &BTreeMap<B256, BTreeMap<B256, U256>>,
        codes: BTreeMap<B256, Bytes>,
    ) -> Self {
        let mut tries = HashMap::new();
        let mut accts = accounts;
        for (h, acc) in &mut accts {
            let values: BTreeMap<B256, Bytes> = storage
                .get(h)
                .into_iter()
                .flatten()
                .filter(|(_, v)| !v.is_zero())
                .map(|(k, v)| (*k, Bytes::from(alloy_rlp::encode(v))))
                .collect();
            if values.is_empty() {
                acc.storage_root = EMPTY_ROOT_HASH;
            } else {
                let trie = Trie::build(&values.iter().map(|(k, v)| (*k, v.to_vec())).collect());
                acc.storage_root = trie.root;
                tries.insert(*h, Slots { values, trie });
            }
        }
        let leaves = accts.iter().map(|(h, a)| (*h, a.rlp())).collect();
        let account_trie = Trie::build(&leaves);
        Self {
            root: account_trie.root,
            accounts: accts,
            account_trie,
            storage: tries,
            codes,
        }
    }

    /// From address-keyed flat tables: addresses and slots are hashed here.
    pub fn from_flat(flat: &impl FlatRead, codes: BTreeMap<B256, Bytes>) -> Self {
        let mut accounts = BTreeMap::new();
        let mut storage: BTreeMap<B256, BTreeMap<B256, U256>> = BTreeMap::new();
        for (a, acc) in flat.accounts() {
            let h = keccak256(a);
            storage.insert(h, flat.slots(a).map(|(s, v)| (keccak256(s), v)).collect());
            accounts.insert(h, acc);
        }
        Self::from_hashed(accounts, &storage, codes)
    }

    /// The head state of a provider. Older blocks have no flat tables to iterate, so they cannot
    /// be served; their roots are answered empty.
    pub fn from_provider<F: FlatWrite>(p: &StateProvider<F>, codes: BTreeMap<B256, Bytes>) -> Self {
        Self::from_flat(p.flat(), codes)
    }

    pub fn root(&self) -> B256 {
        self.root
    }

    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Nodes of the account trie that are hashed (not embedded in their parent).
    pub fn account_node_count(&self) -> usize {
        self.account_trie.nodes_by_hash().count()
    }
}

/// `ServiceGetAccountRangeQuery` (`handlers.go:56-119`).
fn account_range(s: &Snapshot, req: &GetAccountRange) -> AccountRange {
    let empty = AccountRange {
        id: req.id,
        accounts: vec![],
        proof: vec![],
    };
    if req.root != s.root {
        return empty;
    }
    let budget = req.bytes.min(SOFT_RESPONSE_LIMIT);
    let (mut accounts, mut size, mut last) = (Vec::new(), 0u64, None);
    for (hash, acc) in s.accounts.range(req.origin..) {
        let body = Bytes::from(acc.slim_rlp());
        size = size.saturating_add(32 + u64::try_from(body.len()).unwrap_or(u64::MAX));
        last = Some(*hash);
        accounts.push(AccountData { hash: *hash, body });
        if *hash >= req.limit || size > budget {
            break;
        }
    }
    let mut proof = s.account_trie.prove(req.origin);
    if let Some(l) = last {
        proof.extend(s.account_trie.prove(l));
    }
    AccountRange {
        id: req.id,
        accounts,
        proof: dedup(proof),
    }
}

/// `ProofSet`: a node appears once, in the order it was first added.
fn dedup(nodes: Vec<Bytes>) -> Vec<Bytes> {
    let mut seen = std::collections::HashSet::new();
    nodes
        .into_iter()
        .filter(|n| seen.insert(n.clone()))
        .collect()
}

fn bound(b: &Bytes, default: B256) -> Option<B256> {
    match b.len() {
        0 => Some(default),
        32 => Some(B256::from_slice(b)),
        _ => None,
    }
}

/// `ServiceGetStorageRangesQuery` (`handlers.go:172-292`).
fn storage_ranges(s: &Snapshot, req: &GetStorageRanges) -> StorageRanges {
    let empty = StorageRanges {
        id: req.id,
        slots: vec![],
        proof: vec![],
    };
    let (Some(mut origin), Some(mut limit)) = (
        bound(&req.origin, B256::ZERO),
        bound(&req.limit, B256::repeat_byte(0xff)),
    ) else {
        return empty;
    };
    if req.root != s.root {
        return empty;
    }
    let budget = req.bytes.min(SOFT_RESPONSE_LIMIT);
    let hard = hard_limit(budget);
    let (mut sets, mut proof, mut size) = (Vec::new(), Vec::new(), 0u64);
    for account in req.accounts.iter().take(MAX_LOOKUPS) {
        // past the budget: no new range, which would need a proof
        if size >= budget {
            break;
        }
        // only the first account starts elsewhere
        let (o, l) = (
            std::mem::take(&mut origin),
            std::mem::replace(&mut limit, B256::repeat_byte(0xff)),
        );
        if !s.accounts.contains_key(account) {
            return empty; // geth: the iterator fails, the whole answer is empty
        }
        let slots = s.storage.get(account);
        let (mut set, mut last, mut abort) = (Vec::new(), None, false);
        for (hash, body) in slots.iter().flat_map(|x| x.values.range(o..)) {
            if size >= hard {
                abort = true;
                break;
            }
            last = Some(*hash);
            size = size.saturating_add(32 + u64::try_from(body.len()).unwrap_or(u64::MAX));
            set.push(StorageData {
                hash: *hash,
                body: body.clone(),
            });
            if *hash >= l {
                abort = true;
                break;
            }
        }
        let served = !set.is_empty();
        if served {
            sets.push(set);
        }
        // proofs only when the range is partial: it started late or was cut short
        if o != B256::ZERO || (abort && served) {
            if let Some(x) = slots {
                let mut p = x.trie.prove(o);
                if let Some(l) = last {
                    p.extend(x.trie.prove(l));
                }
                proof.extend(dedup(p));
            }
            break;
        }
    }
    StorageRanges {
        id: req.id,
        slots: sets,
        proof,
    }
}

/// `ServiceGetByteCodesQuery` (`handlers.go:348-374`).
fn byte_codes(s: &Snapshot, req: &GetByteCodes) -> ByteCodes {
    let budget = req.bytes.min(SOFT_RESPONSE_LIMIT);
    let (mut codes, mut size) = (Vec::new(), 0u64);
    for h in req.hashes.iter().take(MAX_LOOKUPS) {
        if *h == KECCAK256_EMPTY {
            codes.push(Bytes::new());
        } else if let Some(c) = s.codes.get(h).filter(|c| !c.is_empty()) {
            codes.push(c.clone());
            size = size.saturating_add(u64::try_from(c.len()).unwrap_or(u64::MAX));
        }
        if size > budget {
            break;
        }
    }
    ByteCodes { id: req.id, codes }
}

/// A request that no honest peer sends: the connection is dropped (geth returns the error,
/// `handlers.go:402-404`).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("bad snap request: {0}")]
pub struct BadRequest(pub &'static str);

/// `ServiceGetTrieNodesQuery` (`handlers.go:426-537`). The wall-clock bound of geth (5 s) is not
/// here, there is no clock; the lookup count bounds the work. A path with no node in a trie that
/// exists is answered with an empty string, as geth does (`GetNode` returns no blob and no error,
/// `trie.go:301-305`); a path set of an account that has no storage adds nothing.
fn trie_nodes(s: &Snapshot, req: &GetTrieNodes) -> Result<TrieNodes, BadRequest> {
    let mut out = TrieNodes {
        id: req.id,
        nodes: vec![],
    };
    if req.root != s.root {
        return Ok(out);
    }
    let budget = req.bytes.min(SOFT_RESPONSE_LIMIT);
    let (mut size, mut loads) = (0u64, 0usize);
    let push = |out: &mut TrieNodes, n: Option<&Bytes>, size: &mut u64| {
        let n = n.map_or_else(Bytes::new, Clone::clone);
        *size = size.saturating_add(u64::try_from(n.len()).unwrap_or(u64::MAX));
        out.nodes.push(n);
    };
    for set in &req.paths {
        let Some((first, rest)) = set.split_first() else {
            return Err(BadRequest("zero-item path set"));
        };
        if rest.is_empty() {
            loads += 1;
            let n = from_compact(first).and_then(|p| s.account_trie.node(&p));
            push(&mut out, n, &mut size);
        } else {
            loads += 2;
            let acct = (first.len() == 32)
                .then(|| B256::from_slice(first))
                .and_then(|h| s.storage.get(&h));
            if let Some(x) = acct {
                for path in rest {
                    loads += 1;
                    let n = from_compact(path).and_then(|p| x.trie.node(&p));
                    push(&mut out, n, &mut size);
                    if size > budget || loads > MAX_LOOKUPS {
                        break;
                    }
                }
            }
        }
        if size > budget || loads > MAX_LOOKUPS {
            break;
        }
    }
    Ok(out)
}

/// ADR-007 Q15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub sessions: usize,
    pub per_peer: usize,
    pub bytes_per_sec: u64,
    /// Consecutive rejected requests after which the peer is disconnected.
    pub strikes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            sessions: 4,
            per_peer: 1,
            bytes_per_sec: 16 * 1024 * 1024,
            strikes: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Sessions,
    PerPeer,
    Rate,
}

/// What the caller does with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    Reply(Message),
    /// Over a Q15 bound: send `reply` (empty) and score the peer `RateExceeded`.
    RateExceeded {
        reply: Message,
        why: Reject,
    },
    Disconnect,
}

pub struct Server {
    snap: Snapshot,
    limits: Limits,
    active: Vec<B512>,
    /// Bytes that may still be sent now; negative after a response that went over.
    tokens: i128,
    last_ms: u64,
    strikes: HashMap<B512, u32>,
}

impl Server {
    pub fn new(snap: Snapshot, limits: Limits) -> Self {
        Self {
            snap,
            limits,
            active: Vec::new(),
            tokens: i128::from(limits.bytes_per_sec),
            last_ms: 0,
            strikes: HashMap::new(),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }

    /// Opens a session for `peer`, or says which bound is hit. The rate is a bucket of one
    /// second of bytes that refills in proportion to `now_ms`.
    pub fn admit(&mut self, peer: B512, now_ms: u64) -> Result<(), Reject> {
        let cap = i128::from(self.limits.bytes_per_sec);
        let gained = i128::from(now_ms.saturating_sub(self.last_ms)) * cap / 1000;
        self.tokens = (self.tokens + gained).min(cap);
        self.last_ms = self.last_ms.max(now_ms);
        let mine = self.active.iter().filter(|p| **p == peer).count();
        if mine >= self.limits.per_peer {
            Err(Reject::PerPeer)
        } else if self.active.len() >= self.limits.sessions {
            Err(Reject::Sessions)
        } else if self.tokens <= 0 {
            Err(Reject::Rate)
        } else {
            self.active.push(peer);
            Ok(())
        }
    }

    /// Closes the session of `peer` and charges the bytes that were sent.
    pub fn release(&mut self, peer: B512, sent: usize) {
        if let Some(i) = self.active.iter().position(|p| *p == peer) {
            self.active.swap_remove(i);
        }
        self.tokens -= i128::try_from(sent).unwrap_or(i128::MAX);
    }

    /// The answer to a request, with no admission. `Err` is a request to drop the peer for.
    pub fn respond(&self, msg: &Message) -> Result<Message, BadRequest> {
        Ok(match msg {
            Message::GetAccountRange(r) => Message::AccountRange(account_range(&self.snap, r)),
            Message::GetStorageRanges(r) => Message::StorageRanges(storage_ranges(&self.snap, r)),
            Message::GetByteCodes(r) => Message::ByteCodes(byte_codes(&self.snap, r)),
            Message::GetTrieNodes(r) => Message::TrieNodes(trie_nodes(&self.snap, r)?),
            _ => return Err(BadRequest("not a request")),
        })
    }

    /// Admission, answer, charge. A response message from a peer is unsolicited here.
    pub fn handle(&mut self, peer: B512, now_ms: u64, msg: &Message) -> Handled {
        let Some(empty) = empty_reply(msg) else {
            return Handled::Disconnect;
        };
        if let Err(why) = self.admit(peer, now_ms) {
            let n = self.strikes.entry(peer).or_insert(0);
            *n += 1;
            return if *n > self.limits.strikes {
                self.strikes.remove(&peer);
                Handled::Disconnect
            } else {
                Handled::RateExceeded { reply: empty, why }
            };
        }
        self.strikes.remove(&peer);
        match self.respond(msg) {
            Ok(reply) => {
                self.release(peer, reply.encode().len());
                Handled::Reply(reply)
            }
            Err(_) => {
                self.release(peer, 0);
                Handled::Disconnect
            }
        }
    }
}

fn empty_reply(req: &Message) -> Option<Message> {
    let id = req.id();
    Some(match req {
        Message::GetAccountRange(_) => Message::AccountRange(AccountRange {
            id,
            accounts: vec![],
            proof: vec![],
        }),
        Message::GetStorageRanges(_) => Message::StorageRanges(StorageRanges {
            id,
            slots: vec![],
            proof: vec![],
        }),
        Message::GetByteCodes(_) => Message::ByteCodes(ByteCodes { id, codes: vec![] }),
        Message::GetTrieNodes(_) => Message::TrieNodes(TrieNodes { id, nodes: vec![] }),
        _ => return None,
    })
}

/// The root of leaves that arrive in ascending key order, in O(depth) memory: the builder
/// hashes and drops a subtree as soon as the stream has moved past it (xOneGo `16f60a4f`: an
/// in-memory trie of 100 M slots was killed for memory after a good download).
fn stream_root(leaves: impl Iterator<Item = (B256, Vec<u8>)>) -> B256 {
    let mut hb = HashBuilder::default();
    for (k, v) in leaves {
        hb.add_leaf(Nibbles::unpack(k), &v);
    }
    hb.root()
}

/// The state root of hashed accounts and storage, streamed: for tests and for the final check of
/// a download. A zero slot is absent.
pub fn hashed_root(
    accounts: &BTreeMap<B256, Account>,
    storage: &BTreeMap<B256, BTreeMap<B256, U256>>,
) -> B256 {
    stream_root(accounts.iter().map(|(h, a)| {
        let mut a = a.clone();
        a.storage_root = stream_root(
            storage
                .get(h)
                .into_iter()
                .flatten()
                .filter(|(_, v)| !v.is_zero())
                .map(|(k, v)| (*k, alloy_rlp::encode(v))),
        );
        (*h, a.rlp())
    }))
}
