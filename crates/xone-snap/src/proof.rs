//! Range-proof verification: a contiguous run of leaves of a trie and the nodes along the paths
//! of its two edges prove, against the root, that nothing is missing between `origin` and the
//! last key (geth `trie.VerifyRangeProof`, `trie/proof.go:478-590`).
//!
//! The check is done with alloy-trie's `HashBuilder`: the edge nodes decide, subtree by subtree,
//! what lies left of the range and what lies right of it; those subtrees enter the builder as
//! hashes, the leaves as leaves, and the result must hash to the root. A leaf that is missing,
//! added, changed or moved changes the hash. Rules, with the geth line of each, are in
//! `spec/snap/range-proofs.md`.

use alloy_primitives::{keccak256, Bytes, B256};
use alloy_rlp::Decodable;
use alloy_trie::{
    nodes::{RlpNode, TrieNode},
    HashBuilder, Nibbles,
};
use std::{cmp::Ordering, collections::HashMap};

use crate::mpt::{cmp, cmp_prefix, KEY_NIBBLES};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    #[error("{keys} keys but {values} values")]
    Lengths { keys: usize, values: usize },
    #[error("range is not monotonically increasing")]
    NotIncreasing,
    #[error("range contains an empty value")]
    EmptyValue,
    #[error("unexpected key-value pairs preceding the requested range")]
    BeforeOrigin,
    #[error("a range that does not start at the origin of the trie needs its proof")]
    NoProof,
    #[error("proof node {0} is missing")]
    MissingNode(B256),
    #[error("proof node does not decode: {0}")]
    BadNode(alloy_rlp::Error),
    #[error("proof node cannot occur in a trie of 32-byte keys")]
    Shape,
    #[error("more entries available")]
    MoreEntries,
    #[error("range does not hash to the root")]
    Mismatch,
}

/// Checks `keys`/`values` (a run of leaves, `values` being the leaf bytes) against `root` with
/// the edge proof `proof`, for a request that started at `origin`. Returns whether the trie has
/// more leaves after the last key.
///
/// An empty `proof` is accepted only for a range that starts at the zero hash and rebuilds the
/// whole trie: then the keys and values alone hash to the root, which no truncated range can do.
/// Any other range is refused without its proof.
pub fn verify_range(
    root: B256,
    origin: B256,
    keys: &[B256],
    values: &[Bytes],
    proof: &[Bytes],
) -> Result<bool, ProofError> {
    if keys.len() != values.len() {
        return Err(ProofError::Lengths {
            keys: keys.len(),
            values: values.len(),
        });
    }
    if keys.windows(2).any(|w| w[0] >= w[1]) {
        return Err(ProofError::NotIncreasing);
    }
    if values.iter().any(|v| v.is_empty()) {
        return Err(ProofError::EmptyValue);
    }
    if keys.first().is_some_and(|k| origin > *k) {
        return Err(ProofError::BeforeOrigin);
    }
    let leaves: Vec<(Nibbles, &Bytes)> = keys.iter().map(Nibbles::unpack).zip(values).collect();
    if proof.is_empty() {
        if origin != B256::ZERO {
            return Err(ProofError::NoProof);
        }
        let mut hb = HashBuilder::default();
        for (k, v) in leaves {
            hb.add_leaf(k, v);
        }
        return if hb.root() == root {
            Ok(false)
        } else {
            Err(ProofError::Mismatch)
        };
    }
    let lo = Nibbles::unpack(origin);
    let hi = keys.last().map_or(lo, Nibbles::unpack);
    let nodes: HashMap<B256, &Bytes> = proof.iter().map(|n| (keccak256(n), n)).collect();
    let mut e = Edges {
        nodes,
        lo,
        hi,
        left: Vec::new(),
        right: Vec::new(),
    };
    e.on_path(Nibbles::default(), RlpNode::word_rlp(&root))?;
    let more = !e.right.is_empty();
    if leaves.is_empty() && more {
        return Err(ProofError::MoreEntries);
    }
    // a subtree at the empty path is the whole trie: it cannot sit beside leaves
    if !leaves.is_empty() && e.left.iter().chain(&e.right).any(|(p, _)| p.is_empty()) {
        return Err(ProofError::Mismatch);
    }
    let mut hb = HashBuilder::default();
    let mut prev: Option<Nibbles> = None;
    fn out((p, o): &(Nibbles, Out)) -> (Nibbles, Option<B256>, Option<&[u8]>) {
        match o {
            Out::Hash(h) => (*p, Some(*h), None),
            Out::Leaf(v) => (*p, None, Some(&v[..])),
        }
    }
    let items = e
        .left
        .iter()
        .map(out)
        .chain(leaves.iter().map(|(k, v)| (*k, None, Some(&v[..]))))
        .chain(e.right.iter().map(out));
    for (path, hash, leaf) in items {
        // the builder panics on an out-of-order key; the edges never produce one, but the input
        // is a peer's
        if prev
            .as_ref()
            .is_some_and(|p| cmp(p, &path) != Ordering::Less)
            && !path.is_empty()
        {
            return Err(ProofError::Shape);
        }
        prev = Some(path);
        match (hash, leaf) {
            (Some(h), _) => hb.add_branch(path, h, false),
            (_, Some(v)) => hb.add_leaf(path, v),
            _ => {}
        }
    }
    if hb.root() == root {
        Ok(more)
    } else {
        Err(ProofError::Mismatch)
    }
}

struct Edges<'a> {
    nodes: HashMap<B256, &'a Bytes>,
    lo: Nibbles,
    hi: Nibbles,
    /// Subtrees entirely below `lo` and entirely above `hi`: (path, hash), in key order.
    left: Vec<(Nibbles, Out)>,
    right: Vec<(Nibbles, Out)>,
}

/// A subtree outside the range as the builder takes it: by hash, or, for a node small enough to sit
/// inside its parent's RLP (a leaf with a short value deep in a big trie), as its leaf.
enum Out {
    Hash(B256),
    Leaf(Vec<u8>),
}

impl Edges<'_> {
    /// `path` is a prefix of `lo` or of `hi`, so the node there must be in the proof.
    fn on_path(&mut self, path: Nibbles, r: RlpNode) -> Result<(), ProofError> {
        let (node, hash) = match r.as_hash() {
            Some(h) => {
                let raw = self.nodes.get(&h).ok_or(ProofError::MissingNode(h))?;
                (TrieNode::decode(&mut &raw[..]), Some(h))
            }
            None => (TrieNode::decode(&mut r.as_slice()), None),
        };
        match node.map_err(ProofError::BadNode)? {
            TrieNode::EmptyRoot => {}
            TrieNode::Branch(b) => {
                if path.len() >= KEY_NIBBLES {
                    return Err(ProofError::Shape);
                }
                let mut children = b.stack.into_iter();
                for i in 0..16u8 {
                    if b.state_mask.is_bit_set(i) {
                        let c = children.next().ok_or(ProofError::Shape)?;
                        let mut p = path;
                        p.push(i);
                        self.child(p, c)?;
                    }
                }
            }
            TrieNode::Extension(x) => {
                let p = path.join(&x.key);
                if p.len() >= KEY_NIBBLES {
                    return Err(ProofError::Shape);
                }
                match self.side(&p) {
                    Side::OnPath => self.on_path(p, x.child)?,
                    Side::Inside => {}
                    s => self.outside(s, path, hash)?,
                }
            }
            TrieNode::Leaf(l) => {
                let full = path.join(&l.key);
                if full.len() != KEY_NIBBLES {
                    return Err(ProofError::Shape);
                }
                // a leaf is inside the range when its key is in [lo, hi]
                let s = if cmp(&full, &self.lo) == Ordering::Less {
                    Side::Left
                } else if cmp(&full, &self.hi) == Ordering::Greater {
                    Side::Right
                } else {
                    Side::Inside
                };
                if s != Side::Inside {
                    match hash {
                        Some(_) => self.outside(s, path, hash)?,
                        None => self.push(s, full, Out::Leaf(l.value)),
                    }
                }
            }
        }
        Ok(())
    }

    /// A child slot of an on-path branch. Off-path slots are decided by their prefix alone.
    fn child(&mut self, path: Nibbles, r: RlpNode) -> Result<(), ProofError> {
        match self.side(&path) {
            Side::OnPath => self.on_path(path, r),
            Side::Inside => Ok(()),
            s => match r.as_hash() {
                Some(h) => self.outside(s, path, Some(h)),
                None => match TrieNode::decode(&mut r.as_slice()).map_err(ProofError::BadNode)? {
                    TrieNode::Leaf(l) if path.join(&l.key).len() == KEY_NIBBLES => {
                        self.push(s, path.join(&l.key), Out::Leaf(l.value));
                        Ok(())
                    }
                    _ => Err(ProofError::Shape),
                },
            },
        }
    }

    fn push(&mut self, s: Side, path: Nibbles, o: Out) {
        if s == Side::Left {
            self.left.push((path, o));
        } else {
            self.right.push((path, o));
        }
    }

    fn outside(&mut self, s: Side, path: Nibbles, hash: Option<B256>) -> Result<(), ProofError> {
        // an embedded node (under 32 bytes) has no hash to hand to the builder: a leaf is taken
        // as a leaf (`child`, the leaf arm); an extension cannot be that short with 32-byte keys
        let h = hash.ok_or(ProofError::Shape)?;
        self.push(s, path, Out::Hash(h));
        Ok(())
    }

    /// Where the keys under the prefix `p` lie against the range `[lo, hi]`.
    fn side(&self, p: &Nibbles) -> Side {
        let (a, b) = (cmp_prefix(p, &self.lo), cmp_prefix(p, &self.hi));
        match (a, b) {
            (Ordering::Less, _) => Side::Left,
            (_, Ordering::Greater) => Side::Right,
            (Ordering::Equal, _) | (_, Ordering::Equal) => Side::OnPath,
            _ => Side::Inside,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    OnPath,
    Inside,
}
