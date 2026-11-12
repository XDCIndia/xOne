//! Incremental Merkle-Patricia commitment: a node-cached trie over `alloy_trie` node encoders.
//!
//! [`Trie`] keeps every node with its RLP (or hash) once computed; an insert or a delete clears the
//! cache along the one path it walks, and [`Trie::root`] re-encodes only those nodes. [`StateTrie`]
//! is the account trie plus one [`Trie`] per account with storage, updated from a diff. The
//! reference is [`crate::trie::root_of_hashed`] over the whole table: they must agree after every
//! diff and every unwind (`tests/incremental.rs`).
//!
//! All keys have 64 nibbles, so no key ends inside a branch and a branch never carries a value.

use crate::Account;
use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_trie::{
    nodes::{BranchNodeRef, ExtensionNodeRef, LeafNodeRef, RlpNode},
    Nibbles, TrieMask, EMPTY_ROOT_HASH as EMPTY,
};
use std::collections::{BTreeMap, HashMap};

enum Kind {
    Leaf(Nibbles, Vec<u8>),
    Ext(Nibbles, Box<Node>),
    Branch([Option<Box<Node>>; 16]),
}

struct Node {
    kind: Kind,
    /// The node's reference (inline RLP below 32 bytes, else its hash); `None` after a change.
    rlp: Option<RlpNode>,
}

fn node(kind: Kind) -> Box<Node> {
    Box::new(Node { kind, rlp: None })
}

fn nib(n: &Nibbles, i: usize) -> usize {
    usize::from(n.get_unchecked(i))
}

/// `prefix` then `child` as one node: leaf and extension absorb the path, a branch hangs under it.
fn under(prefix: Nibbles, child: Box<Node>) -> Box<Node> {
    if prefix.is_empty() {
        return child;
    }
    match child.kind {
        Kind::Leaf(p, v) => node(Kind::Leaf(prefix.join(&p), v)),
        Kind::Ext(p, c) => node(Kind::Ext(prefix.join(&p), c)),
        Kind::Branch(_) => node(Kind::Ext(prefix, child)),
    }
}

/// Two entries that differ at their first nibble, as a branch (`a` and `b` are `(nibble, rest)`).
fn fork(a: (usize, Box<Node>), b: (usize, Box<Node>)) -> Box<Node> {
    let mut c: [Option<Box<Node>>; 16] = std::array::from_fn(|_| None);
    c[a.0] = Some(a.1);
    c[b.0] = Some(b.1);
    node(Kind::Branch(c))
}

fn insert(n: Option<Box<Node>>, key: Nibbles, v: Vec<u8>) -> Box<Node> {
    let Some(mut n) = n else {
        return node(Kind::Leaf(key, v));
    };
    n.rlp = None;
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
                return node(Kind::Ext(p, insert(Some(c), key.slice(cp..), v)));
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
            c[i] = Some(insert(c[i].take(), key.slice(1..), v));
            node(Kind::Branch(c))
        }
    }
}

fn remove(mut n: Box<Node>, key: Nibbles) -> Option<Box<Node>> {
    match n.kind {
        Kind::Leaf(ref p, _) => (*p != key).then_some(n),
        Kind::Ext(p, c) => {
            if !key.starts_with(&p) {
                return Some(node(Kind::Ext(p, c)));
            }
            let rest = key.slice(p.len()..);
            remove(c, rest).map(|c| under(p, c))
        }
        Kind::Branch(ref mut c) => {
            let i = nib(&key, 0);
            if let Some(child) = c[i].take() {
                c[i] = remove(child, key.slice(1..));
            }
            n.rlp = None;
            let mut live = c.iter().enumerate().filter(|(_, x)| x.is_some());
            let first = live.next().map(|(i, _)| i);
            match (first, live.next()) {
                (None, _) => None,
                (Some(i), None) => {
                    let only = c[i].take()?;
                    let mut prefix = Nibbles::new();
                    prefix.push(u8::try_from(i).ok()?);
                    Some(under(prefix, only))
                }
                _ => Some(n),
            }
        }
    }
}

fn reference(n: &mut Node) -> RlpNode {
    if let Some(r) = &n.rlp {
        return r.clone();
    }
    let mut buf = Vec::new();
    let r = match &mut n.kind {
        Kind::Leaf(p, v) => LeafNodeRef::new(p, v).rlp(&mut buf),
        Kind::Ext(p, c) => {
            let child = reference(c);
            ExtensionNodeRef::new(p, child.as_slice()).rlp(&mut buf)
        }
        Kind::Branch(c) => {
            let (mut stack, mut mask) = (Vec::new(), TrieMask::default());
            for (i, child) in c.iter_mut().enumerate() {
                if let Some(child) = child {
                    stack.push(reference(child));
                    mask.set_bit(u8::try_from(i).unwrap_or(0));
                }
            }
            BranchNodeRef::new(&stack, mask).rlp(&mut buf)
        }
    };
    n.rlp = Some(r.clone());
    r
}

/// A Merkle-Patricia trie over 32-byte (already hashed) keys with cached node references.
#[derive(Default)]
pub struct Trie {
    root: Option<Box<Node>>,
}

impl Trie {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Sets `key` to `value`, or removes it for `None`.
    pub fn set(&mut self, key: B256, value: Option<Vec<u8>>) {
        let k = Nibbles::unpack(key);
        self.root = match value {
            Some(v) => Some(insert(self.root.take(), k, v)),
            None => self.root.take().and_then(|r| remove(r, k)),
        };
    }

    pub fn get(&self, key: B256) -> Option<&[u8]> {
        let mut k = Nibbles::unpack(key);
        let mut cur = self.root.as_deref()?;
        loop {
            match &cur.kind {
                Kind::Leaf(p, v) => return (*p == k).then_some(v.as_slice()),
                Kind::Ext(p, c) => {
                    if !k.starts_with(p) {
                        return None;
                    }
                    k = k.slice(p.len()..);
                    cur = c;
                }
                Kind::Branch(c) => {
                    cur = c.get(nib(&k, 0))?.as_deref()?;
                    k = k.slice(1..);
                }
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
}

/// What [`StateTrie::apply`] overwrote: put it back with [`StateTrie::undo`].
#[derive(Default)]
pub struct Undo {
    leaves: Vec<(B256, Option<Vec<u8>>)>,
    slots: Vec<(B256, B256, Option<Vec<u8>>)>,
    tries: Vec<(B256, Option<Trie>)>,
    opaque: Vec<(B256, B256)>,
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
                t.set_slot(ha, hs, v);
            }
            acc.storage_root = t.storage_root(ha);
            t.accounts.set(ha, Some(acc.rlp()));
        }
        t
    }

    /// Sets a slot; returns its hashed key and the leaf it replaced.
    fn set_slot(&mut self, ha: B256, hs: B256, v: U256) -> (B256, Option<Vec<u8>>) {
        let tr = self.storage.entry(ha).or_default();
        let old = tr.get(hs).map(<[u8]>::to_vec);
        tr.set(hs, slot_value(v));
        (hs, old)
    }

    fn storage_root(&mut self, ha: B256) -> B256 {
        match self.storage.get_mut(&ha) {
            None => self.opaque.get(&ha).copied().unwrap_or(EMPTY),
            Some(t) if t.is_empty() => {
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
            let old = self.accounts.get(ha).map(<[u8]>::to_vec);
            let listed = accounts.contains_key(&ha);
            if listed && new.is_none() {
                undo.tries.push((ha, self.storage.remove(&ha)));
                if let Some(r) = self.opaque.remove(&ha) {
                    undo.opaque.push((ha, r));
                }
                undo.leaves.push((ha, old));
                self.accounts.set(ha, None);
                continue;
            }
            let mut acc = match (new, &old) {
                (Some(x), _) => x,
                (None, Some(o)) => match alloy_rlp::decode_exact(o) {
                    Ok(x) => x,
                    Err(_) => continue,
                },
                (None, None) => continue,
            };
            for (&(_, hs), &v) in storage.range((ha, B256::ZERO)..=(ha, B256::repeat_byte(0xff))) {
                let (hs, was) = self.set_slot(ha, hs, v);
                undo.slots.push((ha, hs, was));
            }
            acc.storage_root = self.storage_root(ha);
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
        for (ha, t) in u.tries {
            match t {
                Some(t) => self.storage.insert(ha, t),
                None => self.storage.remove(&ha),
            };
        }
        for (ha, s, was) in u.slots.into_iter().rev() {
            let tr = self.storage.entry(ha).or_default();
            tr.set(s, was);
            if tr.is_empty() {
                self.storage.remove(&ha);
            }
        }
        for (ha, was) in u.leaves.into_iter().rev() {
            self.accounts.set(ha, was);
        }
        self.accounts.root()
    }
}
