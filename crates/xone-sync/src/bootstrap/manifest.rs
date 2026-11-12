//! What a snapshot holds and how its anchor is built from a node's own headers
//! (`spec/bootstrap/snapshot-format.md`). The rows are the RLP items of the chunks; [`Side`] is what
//! the executor and the consensus engines keep beside the store and a snapshot has to carry:
//! the code of every contract, the V1 signer snapshot of the pivot, the V2 gap pools and the sign
//! transactions of the blocks inside the window.

use std::collections::BTreeMap;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use xone_chainspec::ChainSpec;
use xone_consensus::v1::checkpoint::{addresses, masternodes_of};
use xone_consensus::v2::epoch_switch::of_header;
use xone_primitives::{Header, WideBalance};

use super::anchor::{Anchor, Provenance, FORMAT_VERSION};

/// An account of the plain state table. The storage root is not carried: it is the root of the
/// slots that follow, and the state root check covers it.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct AccountRow {
    pub address: Address,
    pub nonce: u64,
    /// The true integer, any width up to the legal bound: a wide balance travels in full.
    pub balance: WideBalance,
    pub code_hash: B256,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct SlotRow {
    pub address: Address,
    pub slot: B256,
    pub value: U256,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct CodeRow {
    pub hash: B256,
    pub code: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct RecentRow {
    pub number: u64,
    pub signer: Address,
}

/// A sign transaction (to 0x89) as its block's execution saw it.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct SignRow {
    pub sender: Address,
    pub signed: B256,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct SignsRow {
    pub number: u64,
    pub rows: Vec<SignRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GapRow {
    pub number: u64,
    pub candidates: Vec<Address>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct SideRow {
    pub signers: Vec<Address>,
    pub recents: Vec<RecentRow>,
    pub gaps: Vec<GapRow>,
    pub signs: Vec<SignsRow>,
}

/// A block body of the window. `withdrawals` is the body's list from the fork that adds
/// `withdrawalsHash` to the header (Shanghai; every post-Amsterdam net2040 block has it, empty),
/// each item the withdrawal's RLP inside a string; a row written before the field existed decodes
/// as `None`.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
#[rlp(trailing)]
pub struct BodyRow {
    pub number: u64,
    pub txs: Vec<Bytes>,
    pub uncles: Vec<Header>,
    pub withdrawals: Option<Vec<Bytes>>,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct ReceiptsRow {
    pub number: u64,
    pub receipts: Vec<Bytes>,
}

/// The data of a snapshot that is not the store's. `codes` is every deployed code the state refers
/// to; the rest is as of the pivot (the signs: of the blocks of the window).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Side {
    pub codes: BTreeMap<B256, Bytes>,
    /// V1 only: the signer snapshot after the pivot.
    pub signers: Vec<Address>,
    pub recents: BTreeMap<u64, Address>,
    /// V2 candidate pools by gap block number.
    pub gaps: BTreeMap<u64, Vec<Address>>,
    pub signs: BTreeMap<u64, Vec<SignRow>>,
}

impl Side {
    pub fn row(&self, gaps: &[u64], from: u64) -> SideRow {
        SideRow {
            signers: self.signers.clone(),
            recents: self
                .recents
                .iter()
                .map(|(n, s)| RecentRow {
                    number: *n,
                    signer: *s,
                })
                .collect(),
            gaps: gaps
                .iter()
                .filter_map(|n| {
                    Some(GapRow {
                        number: *n,
                        candidates: self.gaps.get(n)?.clone(),
                    })
                })
                .collect(),
            signs: self
                .signs
                .range(from..)
                .map(|(n, r)| SignsRow {
                    number: *n,
                    rows: r.clone(),
                })
                .collect(),
        }
    }

    pub fn of_row(row: SideRow, codes: BTreeMap<B256, Bytes>) -> Self {
        Self {
            codes,
            signers: row.signers,
            recents: row
                .recents
                .into_iter()
                .map(|r| (r.number, r.signer))
                .collect(),
            gaps: row
                .gaps
                .into_iter()
                .map(|g| (g.number, g.candidates))
                .collect(),
            signs: row.signs.into_iter().map(|s| (s.number, s.rows)).collect(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    #[error("block {0} is not in the store")]
    NoHeader(u64),
    #[error("block {0} is above the head")]
    AboveHead(u64),
    #[error("no proof for block {at}: {why}")]
    NoProof { at: u64, why: &'static str },
}

/// The distinct-signer rule of the V1 proof (`anchor::v1_proof`): `ceil(2n / 3)`.
fn v1_required(n: usize) -> usize {
    (2 * n).div_ceil(3)
}

/// The anchor of block `at`, from the headers `header` gives (canonical, below or at `head`).
/// V2: the nearest epoch switch at or below `at` is the provenance header, the proof runs to
/// `at + 3`. V1: the epoch checkpoint below `at` is the provenance, the proof runs until
/// `ceil(2n / 3)` distinct sealers have signed from `at` up. The result is what `verify_anchor`
/// checks; this builder does not check it (the importer does, against the pins).
pub fn build_anchor(
    spec: &ChainSpec,
    header: &dyn Fn(u64) -> Option<Header>,
    head: u64,
    at: u64,
    now: u64,
    max_age: u64,
) -> Result<Anchor, ProofError> {
    let get = |n: u64| header(n).ok_or(ProofError::NoHeader(n));
    if at > head {
        return Err(ProofError::AboveHead(at));
    }
    let cp = get(at)?;
    let e = spec.xdpos.epoch;
    let no = |why| ProofError::NoProof { at, why };
    let (first, last, validators) = match spec.xdpos.v2.as_ref().filter(|v| at > v.switch_block) {
        Some(v2) => {
            let is_switch = |h: &Header| of_header(v2, e, h).is_ok_and(|s| s.0);
            let mut n = at;
            let sw = loop {
                let h = get(n)?;
                if is_switch(&h) {
                    break h;
                }
                if n <= v2.switch_block + 1 || at - n >= e {
                    return Err(no("no epoch switch at or below it"));
                }
                n -= 1;
            };
            let last = at.checked_add(3).filter(|l| *l <= head).ok_or(no(
                "the head is less than three blocks above it: the 3-chain is not there yet",
            ))?;
            for k in at + 1..=last {
                if is_switch(&get(k)?) {
                    return Err(no("an epoch switch within three blocks above it"));
                }
            }
            let list = addresses(&sw.validators);
            (sw.number, last, list)
        }
        None => {
            if e == 0 || at.is_multiple_of(e) {
                return Err(no("a V1 anchor stands above its epoch checkpoint"));
            }
            let cpn = at - at % e;
            let list =
                masternodes_of(&get(cpn)?.extra_data).map_err(|_| no("no checkpoint list"))?;
            let required = v1_required(list.len());
            let mut seen = std::collections::BTreeSet::new();
            let mut end = None;
            for k in at..=head.min(cpn + e - 1) {
                if let Ok(s) = get(k)?.v1_signer() {
                    seen.insert(s);
                }
                if seen.len() >= required {
                    end = Some(k);
                    break;
                }
            }
            let end = end.ok_or(no(
                "too few distinct sealers above it before the epoch ends or the head",
            ))?;
            (cpn, end, list)
        }
    };
    let mut headers = Vec::new();
    for n in first..=last {
        headers.push(get(n)?);
    }
    let sw = headers.first().ok_or(no("empty proof"))?;
    Ok(Anchor {
        version: FORMAT_VERSION,
        chain_id: spec.chain_id,
        genesis_hash: spec.genesis_hash,
        number: at,
        hash: cp.hash(),
        state_root: cp.state_root,
        created_at: now,
        max_age,
        provenance: Provenance {
            switch_number: sw.number,
            switch_hash: sw.hash(),
            validators,
        },
        headers,
    })
}

/// The default `at` of an export: V2, the latest epoch switch that the 3-chain has committed
/// (at or below `committed` and three below the head); V1, the highest block that has a proof.
pub fn default_at(
    spec: &ChainSpec,
    header: &dyn Fn(u64) -> Option<Header>,
    head: u64,
    committed: Option<u64>,
) -> Result<u64, ProofError> {
    let e = spec.xdpos.epoch;
    let v2 = spec.xdpos.v2.as_ref().filter(|v| head > v.switch_block);
    let (lo, hi) = match (v2, committed) {
        (Some(v), Some(c)) => {
            let hi = c.min(head.saturating_sub(3));
            (
                (v.switch_block + 1).max(hi.saturating_sub(e.saturating_mul(2))),
                hi,
            )
        }
        (Some(_), None) => {
            return Err(ProofError::NoProof {
                at: head,
                why: "no V2 block is committed yet",
            })
        }
        (None, _) => (head.saturating_sub(e.saturating_mul(2)).max(1), head),
    };
    (lo..=hi)
        .rev()
        .find(|&n| build_anchor(spec, header, head, n, 0, 0).is_ok())
        .ok_or(ProofError::NoProof {
            at: head,
            why: "no block has a proof",
        })
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PinsError {
    #[error("line {line}: expected `<number> <0x hash>`")]
    Line { line: usize },
}

/// A pins file: one `<block number> <0x block hash>` per line, `#` starts a comment. A pin is the
/// number and hash of an epoch-switch header (or of the checkpoint): the one fact a manifest cannot
/// carry about itself (`spec/bootstrap-anchor.md`, "What is still trusted").
pub fn parse_pins(text: &str) -> Result<Vec<(u64, B256)>, PinsError> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.split('#').next().map_or("", str::trim);
        if l.is_empty() {
            continue;
        }
        let bad = PinsError::Line { line: i + 1 };
        let mut it = l.split_whitespace();
        let (Some(n), Some(h), None) = (it.next(), it.next(), it.next()) else {
            return Err(bad);
        };
        out.push((
            n.parse().map_err(|_| PinsError::Line { line: i + 1 })?,
            h.parse().map_err(|_| bad)?,
        ));
    }
    Ok(out)
}
