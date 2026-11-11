//! Merkle-Patricia helpers over alloy-trie: a trie that keeps every node by path (for proofs and
//! `GetTrieNodes`), the hex-prefix path of `GetTrieNodes`, and a walk that finds the nodes a trie
//! still lacks. All keys are 32-byte hashes (64 nibbles), as in the account and storage tries.
//!
//! ponytail: [`Trie::build`] hashes the whole leaf set and keeps every node in memory. That is the
//! server's whole-table model, the same as `xone_state::trie::flat_root`. An incremental trie over
//! the MDBX flat tables replaces it together with the one in `xone-state`.

use alloy_primitives::{Bytes, B256};
use alloy_rlp::Decodable;
use alloy_trie::{
    nodes::{RlpNode, TrieNode},
    proof::ProofRetainer,
    HashBuilder, Nibbles, EMPTY_ROOT_HASH,
};
use std::{cmp::Ordering, collections::BTreeMap, collections::HashMap};

pub const KEY_NIBBLES: usize = 64;

/// A trie with every node kept at its path.
#[derive(Debug, Clone, Default)]
pub struct Trie {
    pub root: B256,
    nodes: BTreeMap<Nibbles, Bytes>,
}

impl Trie {
    /// The trie of `leaves` (hashed key to leaf value).
    pub fn build(leaves: &BTreeMap<B256, Vec<u8>>) -> Self {
        if leaves.is_empty() {
            return Self {
                root: EMPTY_ROOT_HASH,
                nodes: BTreeMap::new(),
            };
        }
        // every node is on the path of some leaf, so every node is retained
        let retainer = ProofRetainer::new(leaves.keys().map(Nibbles::unpack).collect());
        let mut hb = HashBuilder::default().with_proof_retainer(retainer);
        for (k, v) in leaves {
            hb.add_leaf(Nibbles::unpack(k), v);
        }
        let root = hb.root();
        Self {
            root,
            nodes: hb.take_proof_nodes().into_inner().into_iter().collect(),
        }
    }

    /// The nodes on the path of `key`, root first, down to the node where the path ends or
    /// diverges (geth `Trie.Prove`, `trie/proof.go:37`). Works for an absent key too.
    pub fn prove(&self, key: B256) -> Vec<Bytes> {
        let full = Nibbles::unpack(key);
        (0..=KEY_NIBBLES)
            .filter_map(|n| self.nodes.get(&full.slice(..n)).cloned())
            .collect()
    }

    /// The node at `path` (geth `Trie.GetNode`, `trie/trie.go:286`).
    pub fn node(&self, path: &Nibbles) -> Option<&Bytes> {
        self.nodes.get(path)
    }

    /// Every node, by hash. Inline nodes (shorter than 32 bytes) are skipped: they are inside
    /// their parent.
    pub fn nodes_by_hash(&self) -> impl Iterator<Item = (B256, &Bytes)> {
        self.nodes
            .values()
            .filter(|n| n.len() >= 32)
            .map(|n| (alloy_primitives::keccak256(n), n))
    }
}

/// Lexicographic order of two nibble paths (a prefix sorts first).
pub fn cmp(a: &Nibbles, b: &Nibbles) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        let o = a.get_unchecked(i).cmp(&b.get_unchecked(i));
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

/// `a` against the first `a.len()` nibbles of `bound`.
pub fn cmp_prefix(a: &Nibbles, bound: &Nibbles) -> Ordering {
    cmp(a, &bound.slice(..a.len().min(bound.len())))
}

/// Hex-prefix ("compact") form of a node path, without the terminator flag
/// (geth `hexToCompact`, `trie/encoding.go:37`).
pub fn compact(path: &Nibbles) -> Bytes {
    let n = path.len();
    let mut out = Vec::with_capacity(n / 2 + 1);
    let mut i = 0;
    if n % 2 == 1 {
        out.push(0x10 | path.get_unchecked(0));
        i = 1;
    } else {
        out.push(0x00);
    }
    while i < n {
        out.push(path.get_unchecked(i) << 4 | path.get_unchecked(i + 1));
        i += 2;
    }
    out.into()
}

/// Inverse of [`compact`] (geth `compactToHex`, `trie/encoding.go:82`). `None` for a path that
/// carries the terminator flag, an odd-flag byte with a non-zero filler or more than 64 nibbles.
/// An empty input is the root, as in geth.
pub fn from_compact(c: &[u8]) -> Option<Nibbles> {
    let Some((&flag, rest)) = c.split_first() else {
        return Some(Nibbles::default());
    };
    if flag > 0x1f || (flag & 0x10 == 0 && flag != 0) || flag & 0x20 != 0 {
        return None;
    }
    let mut nibs = Vec::with_capacity(rest.len() * 2 + 1);
    if flag & 0x10 != 0 {
        nibs.push(flag & 0x0f);
    }
    for b in rest {
        nibs.push(b >> 4);
        nibs.push(b & 0x0f);
    }
    (nibs.len() <= KEY_NIBBLES).then(|| Nibbles::from_nibbles(nibs))
}

/// What a walk found: the nodes it could not resolve and the leaves it reached.
#[derive(Debug, Default)]
pub struct Walk {
    /// Node hash and the path of the node that is missing from `known`.
    pub missing: Vec<(Nibbles, B256)>,
    /// Leaves (full 32-byte key, value) under resolved nodes.
    pub leaves: Vec<(B256, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeError {
    #[error("node does not decode: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("node at a depth or with a key that cannot occur in a trie of 32-byte keys")]
    Shape,
}

/// Walks the trie of `root` through `known` (hash to node RLP), depth first.
pub fn walk(root: B256, known: &HashMap<B256, Bytes>) -> Result<Walk, NodeError> {
    let mut w = Walk::default();
    if root != EMPTY_ROOT_HASH {
        visit(&RlpNode::word_rlp(&root), Nibbles::default(), known, &mut w)?;
    }
    Ok(w)
}

fn visit(
    r: &RlpNode,
    path: Nibbles,
    known: &HashMap<B256, Bytes>,
    w: &mut Walk,
) -> Result<(), NodeError> {
    let bytes: &[u8] = match r.as_hash() {
        Some(h) => match known.get(&h) {
            Some(b) => b,
            None => {
                w.missing.push((path, h));
                return Ok(());
            }
        },
        None => r.as_slice(), // inline node
    };
    match TrieNode::decode(&mut &bytes[..])? {
        TrieNode::EmptyRoot => {}
        TrieNode::Branch(b) => {
            if path.len() >= KEY_NIBBLES {
                return Err(NodeError::Shape);
            }
            let mut children = b.stack.iter();
            for i in 0..16u8 {
                if b.state_mask.is_bit_set(i) {
                    let Some(c) = children.next() else {
                        return Err(NodeError::Shape);
                    };
                    let mut p = path;
                    p.push(i);
                    visit(c, p, known, w)?;
                }
            }
        }
        TrieNode::Extension(e) => {
            let p = path.join(&e.key);
            if p.len() >= KEY_NIBBLES {
                return Err(NodeError::Shape);
            }
            visit(&e.child, p, known, w)?;
        }
        TrieNode::Leaf(l) => {
            let full = path.join(&l.key);
            if full.len() != KEY_NIBBLES {
                return Err(NodeError::Shape);
            }
            let mut key = [0u8; 32];
            full.pack_to(&mut key);
            w.leaves.push((B256::from(key), l.value));
        }
    }
    Ok(())
}
