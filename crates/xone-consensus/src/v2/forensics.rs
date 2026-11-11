//! Forensics of conflicting QCs (geth `engine_v2/forensics.go:50-211`, `:213-431`).
//!
//! When a block is committed by the 3-chain rule the machine remembers the three QCs of the commit
//! (`SetCommittedQCs`, `:56`); when the next block is committed, the new QC and its two ancestors are
//! compared with them. If they are not on one chain, the two QCs that prove the fork are found and one
//! [`Proof`] is made, with the id geth gives it (`generateForensicsId`, `:420`): the diverging block
//! hash, then the hash of each QC's block, smaller round first, joined with `:`.
//!
//! One proof per id: a conflict seen again is not reported again. Memory is bounded by [`SEEN`] ids.
//! The vote-equivocation proof (`ProcessVoteEquivocation`, `:443`) is not made here.

use std::collections::VecDeque;

use alloy_primitives::{Address, B256};
use xone_primitives::{BlockInfo, QuorumCert};

use super::{
    bft::{Chain, HeaderFacts},
    cert::{qc_digest, Scheme},
    Error,
};

/// Ids remembered.
pub const SEEN: usize = 256;
/// geth `forensicsMaxAncestorWalk` (`:321`).
const MAX_ANCESTOR_WALK: usize = 1800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// Block hashes from the diverging block to the QC's block.
    pub path: Vec<B256>,
    pub qc: QuorumCert,
    /// The signers of the QC that recover (geth `getQcSignerAddresses`).
    pub signers: Vec<Address>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub id: String,
    pub diverging_hash: B256,
    pub diverging_number: u64,
    /// The two QCs are in different epochs.
    pub across_epoch: bool,
    pub smaller: Side,
    pub larger: Side,
}

#[derive(Debug, Default, Clone)]
pub struct Forensics {
    /// The QCs of the last commit, oldest first; empty before the first commit.
    committed: Vec<QuorumCert>,
    seen: VecDeque<String>,
}

/// geth `generateForensicsId` (`:420`): `Hash.Hex()` is `0x` and lower-case hex.
pub fn forensics_id(diverging: &B256, qc1: &BlockInfo, qc2: &BlockInfo) -> String {
    format!("{diverging:#x}:{:#x}:{:#x}", qc1.hash, qc2.hash)
}

impl Forensics {
    /// geth `ForensicsMonitoring` (`:50`): compare the incoming QC with the committed ones, then
    /// remember it. `parent` and `proposed` are the two headers above the committed block.
    pub fn monitor<C: Chain, S: Scheme>(
        &mut self,
        c: &C,
        scheme: &S,
        parent: &HeaderFacts,
        proposed: &HeaderFacts,
        incoming: &QuorumCert,
    ) -> Option<Proof> {
        let proof = self.process(c, scheme, incoming).ok().flatten();
        self.set_committed(parent, proposed, incoming);
        let proof = proof?;
        if self.seen.contains(&proof.id) {
            return None;
        }
        if self.seen.len() == SEEN {
            self.seen.pop_front();
        }
        self.seen.push_back(proof.id.clone());
        Some(proof)
    }

    /// geth `SetCommittedQCs` (`:56`): the headers must be on one chain in order, the last one the
    /// block of the incoming QC; otherwise nothing is stored.
    fn set_committed(
        &mut self,
        parent: &HeaderFacts,
        proposed: &HeaderFacts,
        incoming: &QuorumCert,
    ) {
        let (Some(q0), Some(q1)) = (&parent.qc, &proposed.qc) else {
            return;
        };
        if q1.proposed_block_info.hash != parent.hash
            || incoming.proposed_block_info.hash != proposed.hash
        {
            return;
        }
        self.committed = vec![q0.clone(), q1.clone(), incoming.clone()];
    }

    /// geth `ProcessForensics` (`:98`).
    fn process<C: Chain, S: Scheme>(
        &self,
        c: &C,
        scheme: &S,
        incoming: &QuorumCert,
    ) -> Result<Option<Proof>, Error> {
        if self.committed.len() != 3 {
            return Ok(None);
        }
        let set = ancestors(c, incoming)?;
        if same_chain(c, &self.committed, &set)? {
            return Ok(None);
        }
        let same_round = self.committed.iter().find_map(|a| {
            set.iter()
                .find(|b| a.proposed_block_info.round == b.proposed_block_info.round)
                .map(|b| (a, b))
        });
        let proof = match same_round {
            Some((a, b)) => make_proof(c, scheme, a, b)?,
            None => {
                let (anc, lower) = ancestor_through_round(c, &self.committed, &set)?;
                make_proof(c, scheme, &anc, &lower)?
            }
        };
        Ok(Some(proof))
    }
}

fn qc_of(c: &impl Chain, hash: &B256) -> Result<QuorumCert, Error> {
    let h = c.header(hash).ok_or(Error::MissingHeader { hash: *hash })?;
    h.qc.clone().ok_or(Error::Missing {
        what: "QC of a block",
        number: h.number,
    })
}

/// geth `findAncestorQCs` with distance 2 (`:213`): the QC in the block of `qc`, and the QC in the
/// block of that one, oldest first and `qc` last.
fn ancestors(c: &impl Chain, qc: &QuorumCert) -> Result<Vec<QuorumCert>, Error> {
    let q1 = qc_of(c, &qc.proposed_block_info.hash)?;
    let q0 = qc_of(c, &q1.proposed_block_info.hash)?;
    Ok(vec![q0, q1, qc.clone()])
}

/// geth `checkQCsOnTheSameChain` (`:248`): walk the higher set's first QC down to the number of the
/// lower set's first QC and compare the block infos.
fn same_chain(
    c: &impl Chain,
    committed: &[QuorumCert],
    incoming: &[QuorumCert],
) -> Result<bool, Error> {
    let (lower, higher) =
        if incoming[0].proposed_block_info.number < committed[0].proposed_block_info.number {
            (incoming, committed)
        } else {
            (committed, incoming)
        };
    let mut info = higher[0].proposed_block_info.clone();
    let steps = higher[0]
        .proposed_block_info
        .number
        .saturating_sub(lower[0].proposed_block_info.number);
    for _ in 0..steps {
        info = qc_of(c, &info.hash)?.proposed_block_info;
    }
    Ok(info == lower[0].proposed_block_info)
}

/// geth `findAncestorQcThroughRound` (`:323`): the QC of the higher-round set that is the last one
/// whose round is not below the third QC of the lower-round set, and that third QC.
fn ancestor_through_round(
    c: &impl Chain,
    committed: &[QuorumCert],
    incoming: &[QuorumCert],
) -> Result<(QuorumCert, QuorumCert), Error> {
    let (lower, higher) =
        if incoming[0].proposed_block_info.round < committed[0].proposed_block_info.round {
            (incoming, committed)
        } else {
            (committed, incoming)
        };
    let target = lower[2].proposed_block_info.round;
    let mut anc = higher[0].clone();
    for _ in 0..MAX_ANCESTOR_WALK {
        if anc.proposed_block_info.round < target {
            break;
        }
        let next = qc_of(c, &anc.proposed_block_info.hash)?;
        if next.proposed_block_info.round < target {
            return Ok((anc, lower[2].clone()));
        }
        anc = next;
    }
    Err(Error::Missing {
        what: "ancestor QC",
        number: 0,
    })
}

/// geth `FindAncestorBlockHash` (`:364`): the common ancestor of two blocks and the paths to each, in the
/// order of the arguments.
fn find_ancestor(
    c: &impl Chain,
    first: &BlockInfo,
    second: &BlockInfo,
) -> Result<(B256, Vec<B256>, Vec<B256>), Error> {
    let swapped = second.number < first.number;
    let (mut lo, mut hi) = if swapped {
        (second.hash, first.hash)
    } else {
        (first.hash, second.hash)
    };
    let diff = first.number.abs_diff(second.number);
    let (mut lo_path, mut hi_path) = (vec![lo], vec![hi]);
    let parent = |h: &B256| {
        c.header(h)
            .map(|h| h.parent)
            .ok_or(Error::MissingHeader { hash: *h })
    };
    for _ in 0..diff {
        hi = parent(&hi)?;
        hi_path.push(hi);
    }
    while lo != hi {
        let (l, h) = (parent(&lo)?, parent(&hi)?);
        (lo, hi) = (l, h);
        lo_path.push(lo);
        hi_path.push(hi);
    }
    lo_path.reverse();
    hi_path.reverse();
    Ok(if swapped {
        (lo, hi_path, lo_path)
    } else {
        (lo, lo_path, hi_path)
    })
}

/// geth `SendForensicProof` (`:142`).
fn make_proof<C: Chain, S: Scheme>(
    c: &C,
    scheme: &S,
    first: &QuorumCert,
    second: &QuorumCert,
) -> Result<Proof, Error> {
    let (lower, higher) = if second.proposed_block_info.round < first.proposed_block_info.round {
        (second, first)
    } else {
        (first, second)
    };
    let (li, hi) = (&lower.proposed_block_info, &higher.proposed_block_info);
    let (ancestor, lower_path, higher_path) = find_ancestor(c, li, hi)?;
    let across_epoch = c.epoch(&li.hash)?.switch != c.epoch(&hi.hash)?.switch;
    let anc = c
        .header(&ancestor)
        .ok_or(Error::MissingHeader { hash: ancestor })?;
    let side = |path: Vec<B256>, qc: &QuorumCert| Side {
        path,
        signers: qc
            .signatures
            .iter()
            .filter_map(|s| scheme.recover(&qc_digest(qc), s).ok())
            .collect(),
        qc: qc.clone(),
    };
    Ok(Proof {
        id: forensics_id(&ancestor, li, hi),
        diverging_hash: ancestor,
        diverging_number: anc.number,
        across_epoch,
        smaller: side(lower_path, lower),
        larger: side(higher_path, higher),
    })
}
