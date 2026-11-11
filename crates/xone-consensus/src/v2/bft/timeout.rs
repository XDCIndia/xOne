//! Timeouts: geth `TimeoutHandler` and `timeoutHandler` (`engine_v2/timeout.go:93-146`),
//! `onTimeoutPoolThresholdReached` (`:151`), `sendTimeout` (`:225`) and `OnCountdownTimeout` (`:300`).

use std::slice::from_ref;

use xone_primitives::{Timeout, TimeoutCert};

use super::{Bft, Chain, Effect, Outbound, Peer};
use crate::v2::{
    cert::{min_signers, timeout_digest, unique_signers_with, Scheme},
    safety::{Intent, Kind},
    syncinfo,
};

/// A timeout of the current round. Only the current round counts (`timeout.go:101`), and that test
/// comes before any signature work.
pub(super) fn on_timeout<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    t: Timeout,
    from: Option<Peer>,
    fx: &mut Vec<Effect>,
) {
    if t.round != m.round {
        return;
    }
    // geth checks the signer against `NextEpochCandidates` of the gap snapshot, and an empty list is an error
    let Some(candidates) = c.candidates(t.gap_number).filter(|l| !l.is_empty()) else {
        return;
    };
    let digest = timeout_digest(t.round, t.gap_number);
    let signer = match unique_signers_with(&m.scheme, &digest, from_ref(&t.signature), &candidates)
    {
        Ok(s) => match s.into_iter().next() {
            Some(a) => a,
            None => return,
        },
        Err(e) => return super::score(from, &e, fx),
    };
    let (round, gap) = (t.round, t.gap_number);
    if !m.timeouts.add(round, signer, t) {
        return;
    }
    // first gate: the masternodes of the head's epoch (`timeout.go:114-124`)
    let Some(head) = c.head() else {
        return;
    };
    let (Ok(head_epoch), Ok(tc_epoch)) = (c.epoch(&head.hash), c.epoch_of_round(round)) else {
        return;
    };
    let Some(row) = m.cfg.v2.config_at_round(round) else {
        return;
    };
    let (Ok(need_head), Ok(need_tc)) = (
        min_signers(head_epoch.masternodes.len(), row.cert_threshold),
        min_signers(tc_epoch.masternodes.len(), row.cert_threshold),
    ) else {
        return;
    };
    let same_gap = |e: &&Timeout| e.gap_number == gap;
    let counted = m
        .timeouts
        .entries(round)
        .filter(|(a, e)| same_gap(e) && head_epoch.masternodes.contains(a))
        .count();
    if counted < need_head {
        return;
    }
    // second gate: only the active masternodes of the round's epoch sign the TC (`timeout.go:158-196`)
    let signatures: Vec<_> = m
        .timeouts
        .entries(round)
        .filter(|(a, e)| same_gap(e) && tc_epoch.masternodes.contains(a))
        .map(|(_, e)| e.signature.clone())
        .collect();
    if signatures.len() < need_tc {
        return;
    }
    let tc = TimeoutCert {
        round,
        signatures,
        gap_number: gap,
    };
    if m.process_tc(&tc, fx).is_ok() {
        fx.push(Effect::Send(Outbound::SyncInfo(syncinfo::originate(m))));
    }
}

/// The round timer expired (geth `OnCountdownTimeout`). A validator signs and sends the timeout of the
/// current round, counts it, and every `TimeoutSyncThreshold` timeouts sends its SyncInfo
/// (xOneGo #623). The timer restarts whether or not anything was sent (`countdown.go:139-145`).
pub(super) fn on_timer<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    token: u64,
    fx: &mut Vec<Effect>,
) {
    if token != m.timer.token() {
        return;
    }
    let allowed = match (m.cfg.me, c.head()) {
        (Some(me), Some(head)) => c
            .epoch(&head.hash)
            .is_ok_and(|e| e.masternodes.contains(&me)),
        _ => false,
    };
    if allowed && send_timeout(m, c, fx) {
        m.timeout_count += 1;
        let threshold = m
            .cfg
            .v2
            .config_at_round(m.round)
            .map(|r| r.timeout_sync_threshold);
        // a threshold of 0 makes geth divide by zero; here it sends no periodic SyncInfo
        if threshold.is_some_and(|t| m.timeout_count.checked_rem(t) == Some(0)) {
            fx.push(Effect::Send(Outbound::SyncInfo(syncinfo::originate(m))));
        }
    }
    let (token, after) = m.timer.fire();
    fx.push(Effect::SetTimer { token, after });
}

fn send_timeout<S: Scheme, C: Chain>(m: &mut Bft<S>, c: &C, fx: &mut Vec<Effect>) -> bool {
    let Ok(gap) = c.timeout_gap(m.round) else {
        return false;
    };
    let intent = Intent::new(Kind::Timeout, m.round, timeout_digest(m.round, gap));
    m.authorize(
        intent,
        Outbound::Timeout {
            round: m.round,
            gap,
        },
        fx,
    )
}
