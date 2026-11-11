//! The voting rule (geth `verifyVotingRule` and `isExtendingFromAncestor`, `engine_v2/vote.go:404-462`).
//!
//! A pure function of explicit inputs. It decides whether the proposal may be voted; the safety unit
//! ([`decide`](crate::v2::safety::decide)) repeats the round and lock checks on the record before it
//! lets a signature out, so a mistake here is not enough to sign.

use xone_primitives::BlockInfo;

use super::Chain;
use crate::v2::{safety::BlockRef, Error};

/// Why a proposal is not voted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// `currentRound <= highestVotedRound` (`vote.go:406`).
    AlreadyVoted,
    /// The block's round is not the current round (`vote.go:414`).
    RoundMismatch,
    /// The block does not extend the lock QC and its QC is not above it (`vote.go:438`).
    NotExtended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Vote. `extends_lock` is true only when the vote rests on the proposal extending the lock block.
    Vote {
        extends_lock: bool,
    },
    No(Why),
}

/// geth `verifyVotingRule`: `round` is the current round, `highest_voted_round` the highest round
/// voted, `lock` the lock QC's block (none before the first QC of V2), `block` the proposal and
/// `qc_round` the round of the QC it carries. A header that the ancestor walk needs and does not
/// find is an `Err`, not a "no".
pub fn verify_voting_rule<C: Chain>(
    c: &C,
    round: u64,
    highest_voted_round: u64,
    lock: Option<&BlockRef>,
    block: &BlockInfo,
    qc_round: u64,
) -> Result<Verdict, Error> {
    if round <= highest_voted_round {
        return Ok(Verdict::No(Why::AlreadyVoted));
    }
    if block.round != round {
        return Ok(Verdict::No(Why::RoundMismatch));
    }
    let Some(lock) = lock else {
        return Ok(Verdict::Vote {
            extends_lock: false,
        });
    };
    if qc_round > lock.round {
        return Ok(Verdict::Vote {
            extends_lock: false,
        });
    }
    if is_extending_from_ancestor(c, block, lock)? {
        Ok(Verdict::Vote { extends_lock: true })
    } else {
        Ok(Verdict::No(Why::NotExtended))
    }
}

/// geth `isExtendingFromAncestor` (`vote.go:449-462`): walk `current` down by the difference of the
/// two numbers and compare hashes. A difference below zero walks nothing.
pub fn is_extending_from_ancestor<C: Chain>(
    c: &C,
    current: &BlockInfo,
    ancestor: &BlockRef,
) -> Result<bool, Error> {
    let mut next = current.hash;
    for _ in 0..current.number.saturating_sub(ancestor.number) {
        next = c
            .header(&next)
            .ok_or(Error::MissingHeader { hash: next })?
            .parent;
    }
    Ok(next == ancestor.hash)
}
