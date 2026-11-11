//! Proposals and votes: geth `ProposedBlockHandler` (`utils.go:758-790`), `sendVote` (`vote.go:217`),
//! `VoteHandler` and `voteHandler` (`vote.go:88`, `:138`) and `onVotePoolThresholdReached` (`:343`).

use std::slice::from_ref;

use alloy_primitives::B256;
use xone_primitives::{BlockInfo, QuorumCert, Vote};

use super::{
    rule::{verify_voting_rule, Verdict},
    Bft, Chain, Effect, Outbound, Peer, VOTE_LOOKAHEAD,
};
use crate::v2::{
    cert::{min_signers, unique_signers_with, vote_digest, Scheme},
    safety::Intent,
};

/// The block `hash`, executed by this node, as a proposal. It need not be the head: a block of a side
/// branch at the height of the head is voted like any other (xOneGo `ee9b750f`), because the rule
/// asks for the round, the lock and the ancestry, not for the height.
pub(super) fn on_proposal<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    hash: B256,
    fx: &mut Vec<Effect>,
) {
    let Some(h) = c.header(&hash) else {
        return;
    };
    let Some(qc) = h.qc.clone() else {
        return; // the V2 switch block and V1 blocks are not proposals
    };
    let info = BlockInfo {
        hash,
        round: h.round,
        number: h.number,
    };
    if m.process_qc(c, &qc, fx).is_err() {
        return;
    }
    let Ok(epoch) = c.epoch(&hash) else {
        return;
    };
    // geth `allowedToSend`: the key is a masternode of the block's epoch
    if m.cfg.me.is_some_and(|me| epoch.masternodes.contains(&me)) {
        if info.round > m.round {
            // the TC of the round has not arrived: geth drops the block, here it waits for its round
            // (at most `VOTE_LOOKAHEAD` rounds ahead, one block each)
            if info.round <= m.round.saturating_add(VOTE_LOOKAHEAD) {
                m.stash.entry(info.round).or_insert(hash);
            }
        } else if let Ok(Verdict::Vote { extends_lock }) = verify_voting_rule(
            c,
            m.round,
            m.rec.highest_voted_round,
            m.rec.lock_qc.as_ref(),
            &info,
            qc.proposed_block_info.round,
        ) {
            let digest = vote_digest(&info, epoch.gap);
            let intent = Intent::vote(
                info.round,
                digest,
                qc.proposed_block_info.round,
                extends_lock,
            );
            let out = Outbound::Vote {
                info: info.clone(),
                gap: epoch.gap,
            };
            m.authorize(intent, out, fx);
        }
    }
    // votes that came before the block can make its QC now (geth waits for one more vote, `vote.go:189`)
    if info.round >= m.round {
        form_qc(m, c, &info, epoch.gap, fx);
    }
}

/// geth `VoteHandler`: round window, signer, pool, then the QC when enough distinct masternodes voted.
pub(super) fn on_vote<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    v: Vote,
    from: Option<Peer>,
    fx: &mut Vec<Effect>,
) {
    let info = &v.proposed_block_info;
    if info.round < m.round || info.round > m.round.saturating_add(VOTE_LOOKAHEAD) {
        return; // `voteRoundInRange`, before any signature work (`vote.go:134`, #1665)
    }
    let Some(candidates) = c.candidates(v.gap_number) else {
        return;
    };
    let digest = vote_digest(info, v.gap_number);
    let signer = match unique_signers_with(&m.scheme, &digest, from_ref(&v.signature), &candidates)
    {
        Ok(s) => match s.into_iter().next() {
            Some(a) => a,
            None => return,
        },
        Err(e) => return super::score(from, &e, fx),
    };
    let (info, gap) = (v.proposed_block_info.clone(), v.gap_number);
    if m.votes.add(info.round, signer, v) {
        form_qc(m, c, &info, gap, fx);
    }
}

/// geth `voteHandler` from the threshold on (`vote.go:174-211`) and `onVotePoolThresholdReached`: the
/// QC of `info` when enough distinct masternodes of its epoch voted for it in `gap`, and its block is
/// here and agrees with `info`.
fn form_qc<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    info: &BlockInfo,
    gap: u64,
    fx: &mut Vec<Effect>,
) {
    let Ok(epoch) = c.epoch(&info.hash) else {
        return;
    };
    let Some(row) = m.cfg.v2.config_at_round(info.round) else {
        return;
    };
    let Ok(need) = min_signers(epoch.masternodes.len(), row.cert_threshold) else {
        return;
    };
    // the votes of this block that come from masternodes of its epoch (geth `countPoolSigners`)
    let signatures: Vec<_> = m
        .votes
        .entries(info.round)
        .filter(|(a, e)| {
            e.gap_number == gap && e.proposed_block_info == *info && epoch.masternodes.contains(a)
        })
        .map(|(_, e)| e.signature.clone())
        .collect();
    if signatures.len() < need || m.verify_block_info(c, info).is_err() {
        return;
    }
    let qc = QuorumCert {
        proposed_block_info: info.clone(),
        signatures,
        gap_number: gap,
    };
    let _ = m.process_qc(c, &qc, fx);
}
