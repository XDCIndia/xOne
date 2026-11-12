//! `xone snapshot export`: a node writes an anchored snapshot of its own state at block `at`
//! (`spec/bootstrap/snapshot-format.md`). The manifest comes first, then the state, the codes, the
//! side data, the headers, the bodies and the receipts that [`pivot_plan`] says the first
//! post-pivot blocks need, then the trailer. Nothing is written for a pivot whose state does not
//! give the pivot header's state root.

use std::io::Write;

use alloy_primitives::{keccak256, B256, KECCAK256_EMPTY};
use alloy_rlp::Encodable;
use xone_chainspec::ChainSpec;
use xone_state::{trie::flat_root, FlatWrite, MemFlat, Missing};

use super::{
    anchor::Anchor,
    format::{len64, Kind, Writer},
    manifest::{
        build_anchor, AccountRow, BodyRow, CodeRow, ProofError, ReceiptsRow, Side, SlotRow,
    },
    plan::pivot_plan,
    retention::{Class, Error as RetentionError},
};
use crate::store::Store;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Proof(#[from] ProofError),
    #[error(transparent)]
    Plan(#[from] RetentionError),
    #[error("block {0} is not canonical in this store")]
    NotCanonical(u64),
    /// The store does not hold the data the snapshot needs: it is itself a pruned datadir, or
    /// `at` is too far below its head.
    #[error("{class:?} of block {block} is not held by this datadir")]
    NotRetained { class: Class, block: u64 },
    #[error("state of block {at}: {source}")]
    State { at: u64, source: Missing },
    #[error("the state of block {at} gives root {got}, its header says {want}")]
    StateRoot { at: u64, want: B256, got: B256 },
    #[error("the code {0} is referred to by the state and is not in the side data")]
    MissingCode(B256),
    #[error("the gap pool of block {0} is needed by the plan and is not in the side data")]
    MissingGap(u64),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub at: u64,
    /// Seconds since the epoch, and how long the manifest says it stays usable.
    pub now: u64,
    pub max_age: u64,
    /// Raw bytes of a chunk before it is closed.
    pub chunk_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub at: u64,
    pub hash: B256,
    pub state_root: B256,
    pub accounts: u64,
    pub slots: u64,
    pub codes: u64,
    pub headers: u64,
    pub bodies: u64,
    pub bytes: u64,
}

fn rows<T: Encodable, W: Write>(
    w: &mut Writer<W>,
    kind: Kind,
    items: impl Iterator<Item = T>,
    budget: usize,
) -> std::io::Result<u64> {
    let (mut buf, mut len, mut n) = (Vec::new(), 0usize, 0u64);
    for it in items {
        len += it.length();
        buf.push(it);
        n += 1;
        if len >= budget {
            w.chunk(kind, &alloy_rlp::encode(&buf))?;
            (buf, len) = (Vec::new(), 0);
        }
    }
    if !buf.is_empty() {
        w.chunk(kind, &alloy_rlp::encode(&buf))?;
    }
    Ok(n)
}

pub fn export<S: Store, W: Write>(
    spec: &ChainSpec,
    store: &S,
    side: &Side,
    p: &Params,
    w: W,
) -> Result<Exported, ExportError> {
    let at = p.at;
    let hash_of = |n: u64| store.canonical_hash(n);
    let header = |n: u64| store.header(hash_of(n)?);
    let (head, _) = store.head();
    let anchor: Anchor = build_anchor(spec, &header, head, at, p.now, p.max_age)?;
    let plan = pivot_plan(spec, &anchor)?;
    let range = |c: Class| plan.of(c).next().map(|n| (n.first, n.last));
    let pivot = header(at).ok_or(ExportError::NotCanonical(at))?;
    let held = |class, block| ExportError::NotRetained { class, block };

    // state, checked against the pivot header before anything is written
    let rows_at = store
        .state_rows(anchor.hash)
        .map_err(|source| ExportError::State { at, source })?;
    let mut flat = MemFlat::default();
    for (a, x) in &rows_at.accounts {
        flat.set_account(*a, Some(x.clone()));
    }
    for (a, k, v) in &rows_at.storage {
        flat.set_slot(*a, *k, *v);
    }
    let got = flat_root(&flat);
    if got != pivot.state_root {
        return Err(ExportError::StateRoot {
            at,
            want: pivot.state_root,
            got,
        });
    }
    let mut accounts = rows_at.accounts;
    accounts.sort_by_key(|(a, _)| keccak256(a));
    let mut slots = rows_at.storage;
    slots.sort_by_key(|(a, k, _)| (keccak256(a), keccak256(k)));
    let mut codes = Vec::new();
    for h in accounts
        .iter()
        .map(|(_, x)| x.code_hash)
        .filter(|h| *h != KECCAK256_EMPTY)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let code = side.codes.get(&h).ok_or(ExportError::MissingCode(h))?;
        codes.push(CodeRow {
            hash: h,
            code: code.clone(),
        });
    }
    let gaps: Vec<u64> = plan.of(Class::GapSnapshot).map(|n| n.first).collect();
    if let Some(g) = gaps.iter().find(|g| !side.gaps.contains_key(g)) {
        return Err(ExportError::MissingGap(*g));
    }
    // the window, read before the first byte is written
    let (h_first, _) = range(Class::Header).unwrap_or((at, at));
    let r_first = [Class::Body, Class::Receipt]
        .into_iter()
        .filter_map(range)
        .map(|r| r.0)
        .min()
        .unwrap_or(at)
        .min(at);
    let mut headers = Vec::new();
    for n in h_first..=at {
        headers.push(header(n).ok_or(held(Class::Header, n))?);
    }
    let mut bodies = Vec::new();
    let mut receipts = Vec::new();
    for n in r_first..=at {
        let rec = hash_of(n)
            .and_then(|h| store.record(h))
            .ok_or(held(Class::Body, n))?;
        bodies.push(BodyRow {
            number: n,
            txs: rec.block.body.transactions.clone(),
            uncles: rec.block.body.uncles.clone(),
        });
        receipts.push(ReceiptsRow {
            number: n,
            receipts: rec.receipts,
        });
    }

    let manifest = anchor.encode();
    let mut w = Writer::new(w)?;
    w.chunk(Kind::Manifest, &manifest)?;
    let n_accounts = rows(
        &mut w,
        Kind::Accounts,
        accounts.iter().map(|(a, x)| AccountRow {
            address: *a,
            nonce: x.nonce,
            balance: x.balance.clone(),
            code_hash: x.code_hash,
        }),
        p.chunk_bytes,
    )?;
    let n_slots = rows(
        &mut w,
        Kind::Storage,
        slots.iter().map(|(a, k, v)| SlotRow {
            address: *a,
            slot: *k,
            value: *v,
        }),
        p.chunk_bytes,
    )?;
    let n_codes = rows(&mut w, Kind::Codes, codes.into_iter(), p.chunk_bytes)?;
    w.chunk(Kind::Side, &alloy_rlp::encode(side.row(&gaps, r_first)))?;
    rows(&mut w, Kind::Headers, headers.iter(), p.chunk_bytes)?;
    rows(&mut w, Kind::Bodies, bodies.iter(), p.chunk_bytes)?;
    rows(&mut w, Kind::Receipts, receipts.iter(), p.chunk_bytes)?;
    let bytes = w.finish(keccak256(&manifest), h_first, r_first)?;
    Ok(Exported {
        at,
        hash: anchor.hash,
        state_root: anchor.state_root,
        accounts: n_accounts,
        slots: n_slots,
        codes: n_codes,
        headers: len64(headers.len()),
        bodies: len64(bodies.len()),
        bytes,
    })
}
