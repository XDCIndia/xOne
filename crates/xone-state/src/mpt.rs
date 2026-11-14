//! Incremental Merkle-Patricia commitment: a node-cached trie over `alloy_trie` node encoders.
//!
//! [`Trie`] keeps every node with its RLP (or hash) once computed; an insert or a delete clears the
//! cache along the one path it walks, and [`Trie::root`] re-encodes only those nodes. [`StateTrie`]
//! is the account trie plus one [`Trie`] per account with storage, updated from a diff. The
//! reference is [`crate::trie::root_of_hashed`] over the whole table: they must agree after every
//! diff and every unwind (`tests/incremental.rs`).
//!
//! All keys have 64 nibbles, so no key ends inside a branch and a branch never carries a value.
//!
//! # Persisted nodes (`--state.disk`)
//!
//! With [`NodeRows`] a trie keeps its nodes of 32 bytes or more in rows keyed by position (the
//! owner prefix, then the nibbles from the root: geth's pathdb layout). A child that is on disk is a
//! `Stub`, only its reference, until a walk needs it (`load`: the row must hash to the reference,
//! else the trie is [`Trie::failed`]). [`StateTrie::collect`] gives the rows a block changed and the
//! rows of the nodes that went away; [`StateTrie::finish`], after the store committed them, turns
//! the clean nodes below [`PRUNE_DEPTH`] back into stubs and drops the storage tries, so memory does
//! not grow with the state. A node of less than 32 bytes lives inside its parent's row.

use crate::Account;
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_rlp::Decodable;
use alloy_trie::{
    nodes::{BranchNodeRef, ExtensionNodeRef, LeafNodeRef, RlpNode, TrieNode},
    Nibbles, TrieMask, EMPTY_ROOT_HASH as EMPTY,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

/// Nodes at this depth (nibbles from the root) or below, and clean, become stubs after a flush.
pub const PRUNE_DEPTH: usize = 5;

/// Row key prefix of the account trie; a storage trie's is `[1] ++ keccak(address)`.
pub const ACCOUNT_PREFIX: u8 = 0;
pub const STORAGE_PREFIX: u8 = 1;

/// Where a persisted trie reads its nodes from.
pub trait NodeRows: Send + Sync {
    /// The row at `key`. A failed read answers `None` (the trie then reports [`Trie::failed`]).
    fn node(&self, key: &[u8]) -> Option<Vec<u8>>;
}

enum Kind {
    Leaf(Nibbles, Vec<u8>),
    Ext(Nibbles, Box<Node>),
    Branch([Option<Box<Node>>; 16]),
    /// A node of 32 bytes or more that is in a row and not in memory; `rlp` holds its hash.
    Stub,
}

struct Node {
    kind: Kind,
    /// The node's reference (inline RLP below 32 bytes, else its hash); `None` after a change.
    rlp: Option<RlpNode>,
    /// Changed since the last flush: its row is out of date.
    dirty: bool,
}

fn node(kind: Kind) -> Box<Node> {
    Box::new(Node {
        kind,
        rlp: None,
        dirty: true,
    })
}

fn nib(n: &Nibbles, i: usize) -> usize {
    usize::from(n.get_unchecked(i))
}

/// What a trie needs besides its nodes to be persisted.
#[derive(Default)]
struct Walk {
    rows: Option<Arc<dyn NodeRows>>,
    /// Row key prefix (owner).
    prefix: Vec<u8>,
    /// Positions of the nodes a change walked through or absorbed, before the change: their rows
    /// are deleted at the flush unless a node is at that position again.
    touched: BTreeSet<Vec<u8>>,
    failed: bool,
}

impl Walk {
    fn key(&self, pos: &Nibbles) -> Vec<u8> {
        [self.prefix.as_slice(), pos.to_vec().as_slice()].concat()
    }

    fn touch(&mut self, pos: &Nibbles) {
        if self.rows.is_some() {
            self.touched.insert(pos.to_vec());
        }
    }
}

fn child_pos(pos: &Nibbles, i: usize) -> Nibbles {
    let mut p = *pos;
    p.push(u8::try_from(i).unwrap_or(0));
    p
}

/// The node a reference stands for when it is inline, or a stub when it is a hash.
fn child_of(r: &RlpNode) -> Option<Box<Node>> {
    if r.is_hash() {
        Some(Box::new(Node {
            kind: Kind::Stub,
            rlp: Some(r.clone()),
            dirty: false,
        }))
    } else {
        decode_node(r.as_slice(), Some(r.clone()))
    }
}

/// A node from its RLP. `None` for anything that is not a leaf, extension or branch.
fn decode_node(b: &[u8], rlp: Option<RlpNode>) -> Option<Box<Node>> {
    let kind = match TrieNode::decode(&mut &b[..]).ok()? {
        TrieNode::Leaf(l) => Kind::Leaf(l.key, l.value),
        TrieNode::Extension(e) => Kind::Ext(e.key, child_of(&e.child)?),
        TrieNode::Branch(br) => {
            let mut c: [Option<Box<Node>>; 16] = std::array::from_fn(|_| None);
            let mut stack = br.stack.iter();
            for (i, slot) in c.iter_mut().enumerate() {
                if br.state_mask.is_bit_set(u8::try_from(i).ok()?) {
                    *slot = Some(child_of(stack.next()?)?);
                }
            }
            if stack.next().is_some() {
                return None;
            }
            Kind::Branch(c)
        }
        TrieNode::EmptyRoot => return None,
    };
    Some(Box::new(Node {
        kind,
        rlp,
        dirty: false,
    }))
}

/// Replaces a stub by the node of its row. A row that is missing, or does not hash to the stub's
/// reference, sets `failed` and leaves a leaf that makes the root wrong (the header check refuses).
fn load(n: &mut Node, pos: &Nibbles, w: &mut Walk) {
    if !matches!(n.kind, Kind::Stub) {
        return;
    }
    let want = n.rlp.as_ref().and_then(RlpNode::as_hash);
    let row = w.rows.as_ref().and_then(|r| r.node(&w.key(pos)));
    let loaded = match (&row, want) {
        (Some(b), Some(h)) if keccak256(b) == h => decode_node(b, n.rlp.clone()),
        _ => None,
    };
    match loaded {
        Some(l) => *n = *l,
        None => {
            w.failed = true;
            n.kind = Kind::Leaf(Nibbles::default(), Vec::new());
        }
    }
}

/// `prefix` then `child` as one node: leaf and extension absorb the path, a branch hangs under it.
fn under(prefix: Nibbles, child: Box<Node>) -> Box<Node> {
    if prefix.is_empty() {
        return child;
    }
    match child.kind {
        Kind::Leaf(p, v) => node(Kind::Leaf(prefix.join(&p), v)),
        Kind::Ext(p, c) => node(Kind::Ext(prefix.join(&p), c)),
        // a stub below a prefix is a branch (the child of an extension)
        Kind::Branch(_) | Kind::Stub => node(Kind::Ext(prefix, child)),
    }
}

/// Two entries that differ at their first nibble, as a branch (`a` and `b` are `(nibble, rest)`).
fn fork(a: (usize, Box<Node>), b: (usize, Box<Node>)) -> Box<Node> {
    let mut c: [Option<Box<Node>>; 16] = std::array::from_fn(|_| None);
    c[a.0] = Some(a.1);
    c[b.0] = Some(b.1);
    node(Kind::Branch(c))
}

fn insert(
    n: Option<Box<Node>>,
    pos: &Nibbles,
    key: Nibbles,
    v: Vec<u8>,
    w: &mut Walk,
) -> Box<Node> {
    let Some(mut n) = n else {
        return node(Kind::Leaf(key, v));
    };
    load(&mut n, pos, w);
    w.touch(pos);
    n.rlp = None;
    n.dirty = true;
    match n.kind {
        Kind::Leaf(p, old) => {
            let cp = p.common_prefix_length(&key);
            if cp == p.len() {
                return node(Kind::Leaf(p, v));
            }
            let mine = node(Kind::Leaf(p.slice(cp + 1..), old));
            let new = node(Kind::Leaf(key.slice(cp + 1..), v));
            under(
                key.slice(..cp),
                fork((nib(&p, cp), mine), (nib(&key, cp), new)),
            )
        }
        Kind::Ext(p, c) => {
            let cp = p.common_prefix_length(&key);
            if cp == p.len() {
                let below = pos.join(&p);
                return node(Kind::Ext(p, insert(Some(c), &below, key.slice(cp..), v, w)));
            }
            let mine = under(p.slice(cp + 1..), c);
            let new = node(Kind::Leaf(key.slice(cp + 1..), v));
            under(
                key.slice(..cp),
                fork((nib(&p, cp), mine), (nib(&key, cp), new)),
            )
        }
        Kind::Branch(mut c) => {
            let i = nib(&key, 0);
            let below = child_pos(pos, i);
            c[i] = Some(insert(c[i].take(), &below, key.slice(1..), v, w));
            node(Kind::Branch(c))
        }
        // `load` leaves a leaf where a row was missing
        Kind::Stub => node(Kind::Leaf(key, v)),
    }
}

fn remove(mut n: Box<Node>, pos: &Nibbles, key: Nibbles, w: &mut Walk) -> Option<Box<Node>> {
    load(&mut n, pos, w);
    w.touch(pos);
    // a node that stays is written again (its row is deleted unless it is written)
    n.dirty = true;
    match n.kind {
        Kind::Leaf(ref p, _) => (*p != key).then_some(n),
        Kind::Ext(p, c) => {
            if !key.starts_with(&p) {
                return Some(node(Kind::Ext(p, c)));
            }
            let rest = key.slice(p.len()..);
            let below = pos.join(&p);
            remove(c, &below, rest, w).map(|c| under(p, c))
        }
        Kind::Branch(ref mut c) => {
            let i = nib(&key, 0);
            let below = child_pos(pos, i);
            if let Some(child) = c[i].take() {
                c[i] = remove(child, &below, key.slice(1..), w);
            }
            n.rlp = None;
            n.dirty = true;
            let mut live = c.iter().enumerate().filter(|(_, x)| x.is_some());
            let first = live.next().map(|(i, _)| i);
            match (first, live.next()) {
                (None, _) => None,
                (Some(i), None) => {
                    let mut only = c[i].take()?;
                    let at = child_pos(pos, i);
                    load(&mut only, &at, w);
                    if matches!(only.kind, Kind::Leaf(..) | Kind::Ext(..)) {
                        // the node goes into its parent: its row at `at` goes with it
                        w.touch(&at);
                    }
                    let mut prefix = Nibbles::new();
                    prefix.push(u8::try_from(i).ok()?);
                    Some(under(prefix, only))
                }
                _ => Some(n),
            }
        }
        Kind::Stub => None,
    }
}

/// The RLP of `n` (its children's references are cached by [`reference`]).
fn encode(n: &mut Node) -> Vec<u8> {
    let mut buf = Vec::new();
    match &mut n.kind {
        Kind::Leaf(p, v) => {
            LeafNodeRef::new(p, v).rlp(&mut buf);
        }
        Kind::Ext(p, c) => {
            let child = reference(c);
            ExtensionNodeRef::new(p, child.as_slice()).rlp(&mut buf);
        }
        Kind::Branch(c) => {
            let (mut stack, mut mask) = (Vec::new(), TrieMask::default());
            for (i, child) in c.iter_mut().enumerate() {
                if let Some(child) = child {
                    stack.push(reference(child));
                    mask.set_bit(u8::try_from(i).unwrap_or(0));
                }
            }
            BranchNodeRef::new(&stack, mask).rlp(&mut buf);
        }
        Kind::Stub => {}
    }
    buf
}

fn reference(n: &mut Node) -> RlpNode {
    if let Some(r) = &n.rlp {
        return r.clone();
    }
    let r = RlpNode::from_rlp(&encode(n));
    n.rlp = Some(r.clone());
    r
}

/// A Merkle-Patricia trie over 32-byte (already hashed) keys with cached node references.
#[derive(Default)]
pub struct Trie {
    root: Option<Box<Node>>,
    w: Walk,
}

/// The rows a flush changes, by full key (owner prefix and position).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Flush {
    pub put: Vec<(Vec<u8>, Vec<u8>)>,
    pub del: Vec<Vec<u8>>,
    /// Every row whose key starts with one of these (the storage trie of a deleted account).
    pub del_prefix: Vec<Vec<u8>>,
}

impl Trie {
    /// A persisted trie whose root is `root` (a stub until a walk loads it) over `rows`; its row
    /// keys start with `prefix`.
    pub fn persisted(rows: Arc<dyn NodeRows>, prefix: Vec<u8>, root: B256) -> Self {
        Self {
            root: (root != EMPTY).then(|| {
                Box::new(Node {
                    kind: Kind::Stub,
                    rlp: Some(RlpNode::word_rlp(&root)),
                    dirty: false,
                })
            }),
            w: Walk {
                rows: Some(rows),
                prefix,
                ..Walk::default()
            },
        }
    }

    pub fn is_persisted(&self) -> bool {
        self.w.rows.is_some()
    }

    /// A row was missing or did not match its hash while a walk loaded a node: the root is wrong.
    pub fn failed(&self) -> bool {
        self.w.failed
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Sets `key` to `value`, or removes it for `None`.
    pub fn set(&mut self, key: B256, value: Option<Vec<u8>>) {
        let k = Nibbles::unpack(key);
        let at = Nibbles::default();
        self.root = match value {
            Some(v) => Some(insert(self.root.take(), &at, k, v, &mut self.w)),
            None => self
                .root
                .take()
                .and_then(|r| remove(r, &at, k, &mut self.w)),
        };
    }

    /// The leaf at `key`. Loads the nodes on its path (they stay until the next flush).
    pub fn get(&mut self, key: B256) -> Option<Vec<u8>> {
        let mut k = Nibbles::unpack(key);
        let mut pos = Nibbles::default();
        let mut cur = self.root.as_mut()?;
        loop {
            load(cur, &pos, &mut self.w);
            match &mut cur.kind {
                Kind::Leaf(p, v) => return (*p == k).then(|| v.clone()),
                Kind::Ext(p, c) => {
                    if !k.starts_with(p) {
                        return None;
                    }
                    k = k.slice(p.len()..);
                    pos = pos.join(p);
                    cur = c;
                }
                Kind::Branch(c) => {
                    let i = nib(&k, 0);
                    pos = child_pos(&pos, i);
                    k = k.slice(1..);
                    cur = c.get_mut(i)?.as_mut()?;
                }
                Kind::Stub => return None,
            }
        }
    }

    /// The root hash; only the nodes changed since the last call are encoded again.
    pub fn root(&mut self) -> B256 {
        match self.root.as_deref_mut() {
            None => EMPTY,
            Some(n) => {
                let r = reference(n);
                r.as_hash().unwrap_or_else(|| keccak256(r.as_slice()))
            }
        }
    }

    /// The rows this trie must write so that its rows are the trie again, and the rows to delete.
    /// Changes nothing: [`Self::finish`] does, after the rows are committed.
    pub fn collect(&mut self, out: &mut Flush) {
        self.root();
        let mut written = BTreeSet::new();
        if let Some(r) = self.root.as_deref_mut() {
            collect_node(r, &mut Nibbles::default(), &self.w, &mut written, out);
        }
        for p in &self.w.touched {
            if !written.contains(p) {
                out.del.push([self.w.prefix.as_slice(), p].concat());
            }
        }
    }

    /// The rows are committed: nothing is dirty, and clean nodes at [`PRUNE_DEPTH`] or below are
    /// stubs again.
    pub fn finish(&mut self, prune_depth: usize) {
        self.w.touched.clear();
        if let Some(r) = self.root.as_deref_mut() {
            settle(r, 0, prune_depth);
        }
    }
}

fn collect_node(
    n: &mut Node,
    pos: &mut Nibbles,
    w: &Walk,
    written: &mut BTreeSet<Vec<u8>>,
    out: &mut Flush,
) {
    if !n.dirty {
        return;
    }
    match &mut n.kind {
        Kind::Ext(p, c) => {
            let at = pos.len();
            pos.extend(p);
            collect_node(c, pos, w, written, out);
            pos.truncate(at);
        }
        Kind::Branch(c) => {
            for (i, ch) in c.iter_mut().enumerate() {
                if let Some(ch) = ch {
                    let at = pos.len();
                    pos.push(u8::try_from(i).unwrap_or(0));
                    collect_node(ch, pos, w, written, out);
                    pos.truncate(at);
                }
            }
        }
        Kind::Leaf(..) | Kind::Stub => {}
    }
    if n.rlp.as_ref().is_some_and(RlpNode::is_hash) {
        out.put.push((w.key(pos), encode(n)));
        written.insert(pos.to_vec());
    }
}

fn settle(n: &mut Node, depth: usize, prune: usize) {
    n.dirty = false;
    match &mut n.kind {
        Kind::Ext(p, c) => settle_child(c, depth + p.len(), prune),
        Kind::Branch(c) => {
            for ch in c.iter_mut().flatten() {
                settle_child(ch, depth + 1, prune);
            }
        }
        Kind::Leaf(..) | Kind::Stub => {}
    }
}

fn settle_child(c: &mut Box<Node>, depth: usize, prune: usize) {
    if matches!(c.kind, Kind::Stub) {
        return;
    }
    settle(c, depth, prune);
    if depth >= prune && c.rlp.as_ref().is_some_and(RlpNode::is_hash) {
        c.kind = Kind::Stub;
    }
}

/// What [`StateTrie::apply`] overwrote: put it back with [`StateTrie::undo`].
#[derive(Default)]
pub struct Undo {
    leaves: Vec<(B256, Option<Vec<u8>>)>,
    slots: Vec<(B256, B256, Option<Vec<u8>>)>,
    tries: Vec<(B256, Option<Trie>)>,
    opaque: Vec<(B256, B256)>,
    wiped: Vec<B256>,
}

/// The account trie and the storage trie of each account with storage (keyed by `keccak(address)`).
#[derive(Default)]
pub struct StateTrie {
    accounts: Trie,
    storage: HashMap<B256, Trie>,
    /// Accounts whose storage is not held, only its root (keyed by `keccak(address)`): a pruned
    /// state, for tests. Their storage root is that root until a diff writes one of their slots,
    /// which this trie cannot do (the root is then wrong and the header check says so).
    opaque: HashMap<B256, B256>,
    /// Persisted: the rows. The storage tries in `storage` are those a diff touched since the last
    /// flush; the others are not in memory and their root is the one in the account's leaf.
    rows: Option<Arc<dyn NodeRows>>,
    /// Persisted: accounts deleted since the last flush, whose storage rows go.
    wiped: BTreeSet<B256>,
    /// Persisted: nodes at this depth or below go back to stubs at a flush.
    prune_depth: usize,
}

fn slot_value(v: U256) -> Option<Vec<u8>> {
    (!v.is_zero()).then(|| alloy_rlp::encode(v))
}

impl StateTrie {
    /// The trie of a flat state: every account, with its storage root taken from its slots.
    pub fn from_flat(flat: &impl crate::FlatRead) -> Self {
        Self::from_flat_pruned(flat, &BTreeMap::new())
    }

    /// [`Self::from_flat`] with the storage root of the `opaque` accounts given instead of their slots.
    pub fn from_flat_pruned(flat: &impl crate::FlatRead, opaque: &BTreeMap<Address, B256>) -> Self {
        let mut t = Self {
            opaque: opaque.iter().map(|(a, r)| (keccak256(a), *r)).collect(),
            ..Self::default()
        };
        for (ha, acc) in flat.hashed_accounts() {
            let slots: BTreeMap<_, _> = flat.hashed_slots(ha).collect();
            let mut acc = acc;
            for (hs, v) in slots {
                t.set_slot(ha, hs, v, EMPTY);
            }
            acc.storage_root = t.storage_root(ha, EMPTY);
            t.accounts.set(ha, Some(acc.rlp()));
        }
        t
    }

    /// The persisted trie whose state root is `root`, over `rows`: nothing is loaded yet.
    pub fn persisted(rows: Arc<dyn NodeRows>, root: B256) -> Self {
        Self {
            accounts: Trie::persisted(rows.clone(), vec![ACCOUNT_PREFIX], root),
            rows: Some(rows),
            prune_depth: PRUNE_DEPTH,
            ..Self::default()
        }
    }

    /// Prunes at `depth` instead of [`PRUNE_DEPTH`] (a test makes every node but the root a stub).
    pub fn with_prune_depth(mut self, depth: usize) -> Self {
        self.prune_depth = depth;
        self
    }

    /// Persisted trie rows are used.
    pub fn is_persisted(&self) -> bool {
        self.rows.is_some()
    }

    /// A node row was missing or wrong while a walk loaded it: roots are not to be trusted.
    pub fn failed(&self) -> bool {
        self.accounts.failed() || self.storage.values().any(Trie::failed)
    }

    /// Sets a slot; returns its hashed key and the leaf it replaced. `base` is the storage root of
    /// the account before this change (a persisted trie starts from it).
    fn set_slot(&mut self, ha: B256, hs: B256, v: U256, base: B256) -> (B256, Option<Vec<u8>>) {
        let rows = self.rows.clone();
        let tr = self.storage.entry(ha).or_insert_with(|| match rows {
            Some(r) => Trie::persisted(r, [&[STORAGE_PREFIX][..], ha.as_slice()].concat(), base),
            None => Trie::default(),
        });
        let old = tr.get(hs);
        tr.set(hs, slot_value(v));
        (hs, old)
    }

    fn storage_root(&mut self, ha: B256, base: B256) -> B256 {
        let persisted = self.rows.is_some();
        match self.storage.get_mut(&ha) {
            None if persisted => base,
            None => self.opaque.get(&ha).copied().unwrap_or(EMPTY),
            // a persisted trie stays until the flush: its rows are deleted from it
            Some(t) if t.is_empty() && !persisted => {
                self.storage.remove(&ha);
                EMPTY
            }
            Some(t) => t.root(),
        }
    }

    /// Applies a diff (`accounts`: `None` deletes the account with its storage; `storage`: zero
    /// removes a slot) or a changeset, which has the same shape, and returns the new root. A slot of
    /// an account the diff deletes is skipped, a slot of an account that is not in the trie is not
    /// part of any root (as in [`crate::trie::flat_root`]).
    pub fn apply(
        &mut self,
        accounts: &BTreeMap<B256, Option<Account>>,
        storage: &BTreeMap<(B256, B256), U256>,
    ) -> (B256, Undo) {
        let mut undo = Undo::default();
        let mut touched: BTreeMap<B256, Option<Account>> = BTreeMap::new();
        for (a, _) in storage.keys() {
            touched.entry(*a).or_insert(None);
        }
        for (a, x) in accounts {
            touched.insert(*a, x.clone());
        }
        for (ha, new) in touched {
            let old = self.accounts.get(ha);
            let listed = accounts.contains_key(&ha);
            let old_acc: Option<Account> =
                old.as_deref().and_then(|o| alloy_rlp::decode_exact(o).ok());
            let base = old_acc.as_ref().map_or(EMPTY, |a| a.storage_root);
            if listed && new.is_none() {
                undo.tries.push((ha, self.storage.remove(&ha)));
                if self.rows.is_some() && base != EMPTY && self.wiped.insert(ha) {
                    undo.wiped.push(ha);
                }
                if let Some(r) = self.opaque.remove(&ha) {
                    undo.opaque.push((ha, r));
                }
                undo.leaves.push((ha, old));
                self.accounts.set(ha, None);
                continue;
            }
            let mut acc = match (new, old_acc) {
                (Some(x), _) => x,
                (None, Some(o)) => o,
                (None, None) => continue,
            };
            for (&(_, hs), &v) in storage.range((ha, B256::ZERO)..=(ha, B256::repeat_byte(0xff))) {
                let (hs, was) = self.set_slot(ha, hs, v, base);
                undo.slots.push((ha, hs, was));
            }
            acc.storage_root = self.storage_root(ha, base);
            undo.leaves.push((ha, old));
            self.accounts.set(ha, Some(acc.rlp()));
        }
        (self.accounts.root(), undo)
    }

    pub fn root(&mut self) -> B256 {
        self.accounts.root()
    }

    /// Puts back what `apply` overwrote.
    pub fn undo(&mut self, u: Undo) -> B256 {
        self.opaque.extend(u.opaque);
        for ha in u.wiped {
            self.wiped.remove(&ha);
        }
        for (ha, t) in u.tries {
            match t {
                Some(t) => self.storage.insert(ha, t),
                None => self.storage.remove(&ha),
            };
        }
        let persisted = self.rows.is_some();
        for (ha, s, was) in u.slots.into_iter().rev() {
            if persisted && !self.storage.contains_key(&ha) {
                continue; // a persisted trie stays in the map until the flush
            }
            let tr = self.storage.entry(ha).or_default();
            tr.set(s, was);
            if tr.is_empty() && !persisted {
                self.storage.remove(&ha);
            }
        }
        for (ha, was) in u.leaves.into_iter().rev() {
            self.accounts.set(ha, was);
        }
        self.accounts.root()
    }

    /// The rows that make the persisted trie's rows equal this trie (after a [`Self::apply`] or an
    /// [`Self::undo`] whose result is to be committed): the changed nodes, the nodes that went away,
    /// and the storage tries of deleted accounts. Changes nothing; see [`Self::finish`].
    pub fn collect(&mut self) -> Flush {
        let mut f = Flush::default();
        if self.rows.is_none() {
            return f;
        }
        self.accounts.collect(&mut f);
        for t in self.storage.values_mut() {
            t.collect(&mut f);
        }
        for ha in &self.wiped {
            f.del_prefix
                .push([&[STORAGE_PREFIX][..], ha.as_slice()].concat());
        }
        f
    }

    /// The rows of [`Self::collect`] are committed: the storage tries are dropped, the account trie
    /// below [`PRUNE_DEPTH`] goes back to stubs.
    pub fn finish(&mut self) {
        if self.rows.is_none() {
            return;
        }
        self.accounts.finish(self.prune_depth);
        self.storage.clear();
        self.wiped.clear();
    }
}
