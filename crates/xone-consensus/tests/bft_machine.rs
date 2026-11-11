#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! The BFT step machine on a small chain with real secp256k1 signatures: effect order, certificate
//! formation, timeouts and SyncInfo, SyncInfo that must change nothing (EX-1), seeding after a sync
//! (xOneGo #718), the mine period (#721), the sibling vote, and a flood of variants (XDC-07).

mod bft_common;

use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256};
use bft_common::{keys, row, v2, Key, TestChain};
use xone_chainspec::V2Config;
use xone_consensus::v2::{
    bft::{Bft, Config, Effect, Event, Offence, Outbound},
    cert::{vote_digest, Scheme, Secp},
    safety::{Intent, Kind, Refusal, SafetyRecord},
    Error,
};
use xone_db::Durability;
use xone_primitives::{BlockInfo, Timeout, TimeoutCert, Vote};

const D: Durability = Durability::Durable;

struct Net {
    keys: Vec<Key>,
    chain: TestChain,
    switch: B256,
}

/// The chain of `n` validators with the V2 switch block (number 100) only.
fn net(n: u8) -> Net {
    let keys = keys(n);
    let mut chain = TestChain::new(keys.iter().map(Key::address).collect());
    let switch = chain.add(100, B256::ZERO, 0, None, 0);
    Net {
        keys,
        chain,
        switch,
    }
}

impl Net {
    /// A block on `parent` that carries the QC of the parent signed by `who`.
    fn block(&mut self, parent: B256, round: u64, who: &[usize], salt: u8) -> B256 {
        let info = self.chain.info(parent);
        let qc = if info.round == 0 {
            self.chain.qc(&info, &self.keys, &[])
        } else {
            self.chain.qc(&info, &self.keys, who)
        };
        self.chain
            .add(info.number + 1, parent, round, Some(qc), salt)
    }

    fn machine_with<S: Scheme>(&self, me: usize, rows: &[(u64, V2Config)], scheme: S) -> Bft<S> {
        let cfg = Config {
            v2: v2(rows),
            gap: 450,
            me: Some(self.keys[me].address()),
        };
        Bft::new(cfg, scheme, SafetyRecord::new(self.keys[me].address())).unwrap()
    }

    fn machine(&self, me: usize) -> Bft {
        self.machine_with(me, &[(0, row(10, 3, 2))], Secp)
    }

    fn vote(&self, who: usize, info: &BlockInfo) -> Vote {
        let d = vote_digest(info, self.chain.gap);
        Vote {
            proposed_block_info: info.clone(),
            signature: self.keys[who].sign(&d),
            gap_number: self.chain.gap,
        }
    }

    fn timeout(&self, who: usize, round: u64) -> Timeout {
        let d = xone_consensus::v2::cert::timeout_digest(round, self.chain.gap);
        Timeout {
            round,
            signature: self.keys[who].sign(&d),
            gap_number: self.chain.gap,
        }
    }
}

fn step<S: Scheme>(m: &mut Bft<S>, n: &Net, ev: Event) -> Vec<Effect> {
    let fx = m.step(&n.chain, D, ev);
    assert_persist_before_send(&fx);
    fx
}

/// Every `Send` of a vote or a timeout is right after its `Sign`, and a `PersistIntent` of the same
/// intent comes before that `Sign` (a retry that the record already holds has none).
fn assert_persist_before_send(fx: &[Effect]) {
    for (i, e) in fx.iter().enumerate() {
        if let Effect::Send(Outbound::Vote { .. } | Outbound::Timeout { .. }) = e {
            let Some(Effect::Sign(intent)) = i.checked_sub(1).map(|j| &fx[j]) else {
                panic!("a send without its sign: {fx:?}");
            };
            for (j, p) in fx.iter().enumerate() {
                if let Effect::PersistIntent { intent: p, .. } = p {
                    if p == intent {
                        assert!(j < i - 1, "persist after sign: {fx:?}");
                    }
                }
            }
        }
        if let Effect::Sign(_) = e {
            assert!(
                matches!(fx.get(i + 1), Some(Effect::Send(_))),
                "a sign that is not sent: {fx:?}"
            );
        }
    }
}

fn sends(fx: &[Effect]) -> Vec<&Outbound> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Send(o) => Some(o),
            _ => None,
        })
        .collect()
}

fn rounds(fx: &[Effect]) -> Vec<(u64, u64)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::AdvanceRound { round, mine_period } => Some((*round, *mine_period)),
            _ => None,
        })
        .collect()
}

fn timer(fx: &[Effect]) -> u64 {
    fx.iter()
        .rev()
        .find_map(|e| match e {
            Effect::SetTimer { token, .. } => Some(*token),
            _ => None,
        })
        .expect("a timer")
}

/// What a message must not change: everything the machine holds that a peer could poison.
fn state<S: Scheme>(
    m: &Bft<S>,
) -> (
    u64,
    String,
    String,
    SafetyRecord,
    (usize, usize),
    Option<String>,
) {
    (
        m.round(),
        format!("{:?}", m.highest_qc()),
        format!("{:?}", m.highest_tc()),
        m.record().clone(),
        m.pooled(),
        m.committed().map(|c| format!("{c:?}")),
    )
}

// ---------------------------------------------------------------- the vote path

#[test]
fn a_proposal_is_voted_with_persist_then_sign_then_send_and_only_once() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    let fx = step(&mut m, &n, Event::Synced);
    assert_eq!(
        rounds(&fx),
        [(1, 2)],
        "seeded at the switch block: round 1, minePeriod of the config"
    );
    assert_eq!(m.round(), 1);
    let fx = step(&mut m, &n, Event::Proposal(b1));
    assert!(
        matches!(&fx[..], [Effect::PersistIntent { intent, record }, Effect::Sign(i), Effect::Send(Outbound::Vote { info, .. })]
            if intent == i && intent.kind == Kind::Vote && intent.round == 1 && record.highest_voted_round == 1 && info.hash == b1),
        "{fx:?}"
    );
    assert_eq!(m.record().highest_voted_round, 1);
    assert!(
        step(&mut m, &n, Event::Proposal(b1)).is_empty(),
        "no second vote in the round"
    );
    // a sibling of the same round is not voted either
    let b1x = n.block(n.switch, 1, &[], 9);
    assert!(step(&mut m, &n, Event::Proposal(b1x)).is_empty());
    // a follower never signs
    let cfg = Config {
        v2: v2(&[(0, row(10, 3, 2))]),
        gap: 450,
        me: None,
    };
    let mut f = Bft::new(cfg, Secp, SafetyRecord::new(Address::ZERO)).unwrap();
    assert!(sends(&step(&mut f, &n, Event::Proposal(b1))).is_empty());
}

#[test]
fn a_degraded_store_refuses_every_signature() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    let fx = m.step(
        &n.chain,
        Durability::Degraded(xone_db::Cause::Io),
        Event::Proposal(b1),
    );
    assert!(
        matches!(
            &fx[..],
            [Effect::Refused {
                kind: Kind::Vote,
                why: Refusal::Degraded(_)
            }]
        ),
        "{fx:?}"
    );
}

#[test]
fn enough_distinct_votes_make_a_qc_and_start_the_next_round() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    step(&mut m, &n, Event::Proposal(b1));
    let info = n.chain.info(b1);
    // threshold of 4 masternodes at 0.667 is 3
    for who in [0, 1] {
        let fx = step(&mut m, &n, Event::Vote(n.vote(who, &info), Some(7)));
        assert!(rounds(&fx).is_empty());
    }
    // the same vote again, and from the same signer: not counted
    assert!(step(&mut m, &n, Event::Vote(n.vote(1, &info), Some(7))).is_empty());
    assert_eq!(m.pooled(), (2, 0));
    let fx = step(&mut m, &n, Event::Vote(n.vote(2, &info), Some(8)));
    assert_eq!(rounds(&fx), [(2, 2)]);
    assert_eq!(m.round(), 2);
    assert_eq!(m.highest_qc().proposed_block_info, info);
    assert_eq!(
        m.highest_qc().signatures.len(),
        3,
        "one signature per signer"
    );
    // the next proposal carries this QC and is voted in round 2
    let b2 = n.block(b1, 2, &[0, 1, 2], 0);
    let fx = step(&mut m, &n, Event::Proposal(b2));
    assert!(
        matches!(
            &fx[..],
            [
                Effect::PersistIntent { .. },
                Effect::Sign(_),
                Effect::Send(Outbound::Vote { .. })
            ]
        ),
        "{fx:?}"
    );
}

#[test]
fn a_vote_of_a_non_member_or_a_malformed_signature_is_scored_and_pooled_nowhere() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    let info = n.chain.info(b1);
    let outsider = Key::new(99);
    let d = vote_digest(&info, 0);
    let bad = Vote {
        proposed_block_info: info.clone(),
        signature: outsider.sign(&d),
        gap_number: 0,
    };
    let fx = step(&mut m, &n, Event::Vote(bad, Some(3)));
    assert_eq!(
        fx,
        [Effect::ScorePeer {
            peer: 3,
            offence: Offence::Invalid
        }]
    );
    let junk = Vote {
        proposed_block_info: info,
        signature: Bytes::from(vec![1; 65]),
        gap_number: 0,
    };
    let fx = step(&mut m, &n, Event::Vote(junk, Some(4)));
    assert_eq!(
        fx,
        [Effect::ScorePeer {
            peer: 4,
            offence: Offence::Invalid
        }]
    );
    assert_eq!(m.pooled(), (0, 0));
}

#[test]
fn votes_before_their_block_make_the_qc_when_the_block_is_executed() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    // the block is not yet known to the machine's chain view
    let info = n.chain.info(b1);
    let full = std::mem::take(&mut n.chain.headers);
    let mut with_switch = TestChain::new(n.chain.masternodes.clone());
    with_switch
        .headers
        .insert(n.switch, full[&n.switch].clone());
    with_switch.head = Some(n.switch);
    let mut m = n.machine_with(1, &[(0, row(10, 3, 2))], Secp);
    let early = Net {
        keys: keys(4),
        chain: with_switch,
        switch: n.switch,
    };
    step(&mut m, &early, Event::Synced);
    for who in [0, 2, 3] {
        step(&mut m, &early, Event::Vote(early.vote(who, &info), Some(1)));
    }
    assert_eq!(m.round(), 1, "no block, no QC");
    n.chain.headers = full;
    let fx = step(&mut m, &n, Event::Proposal(b1));
    assert!(rounds(&fx).contains(&(2, 2)), "{fx:?}");
}

// ---------------------------------------------------------------- timeouts and SyncInfo

#[test]
fn a_timeout_is_persisted_signed_sent_again_and_the_timer_restarts() {
    let n = net(4);
    let mut m = n.machine(0);
    let fx = step(&mut m, &n, Event::Synced);
    let tok = timer(&fx);
    let fx = step(&mut m, &n, Event::Timer(tok));
    assert!(
        matches!(&fx[..], [Effect::PersistIntent { intent, .. }, Effect::Sign(_), Effect::Send(Outbound::Timeout { round: 1, .. }), Effect::SetTimer { .. }]
            if intent.kind == Kind::Timeout),
        "{fx:?}"
    );
    let tok2 = timer(&fx);
    assert_ne!(tok, tok2);
    assert!(
        step(&mut m, &n, Event::Timer(tok)).is_empty(),
        "a stale timer is ignored"
    );
    // the repeat of the same timeout is a replay: nothing to persist
    let fx = step(&mut m, &n, Event::Timer(tok2));
    assert!(
        matches!(
            &fx[..],
            [
                Effect::Sign(_),
                Effect::Send(Outbound::Timeout { .. }),
                Effect::SetTimer { .. }
            ]
        ),
        "{fx:?}"
    );
}

#[test]
fn syncinfo_is_sent_after_every_timeout_sync_threshold_timeouts() {
    for threshold in [2u64, 3, 5] {
        let n = net(4);
        let mut m = n.machine_with(0, &[(0, row(10, threshold, 2))], Secp);
        let mut tok = timer(&step(&mut m, &n, Event::Synced));
        for i in 1..=3 * threshold {
            let fx = step(&mut m, &n, Event::Timer(tok));
            tok = timer(&fx);
            let has = sends(&fx)
                .iter()
                .any(|o| matches!(o, Outbound::SyncInfo(_)));
            assert_eq!(
                has,
                i % threshold == 0,
                "threshold {threshold}, timeout {i}"
            );
        }
    }
}

#[test]
fn enough_timeouts_make_a_tc_move_the_round_and_send_syncinfo() {
    let n = net(4);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    for who in [1, 2] {
        assert!(step(&mut m, &n, Event::Timeout(n.timeout(who, 1), Some(5))).is_empty());
    }
    // a timeout of another round is not counted
    assert!(step(&mut m, &n, Event::Timeout(n.timeout(3, 2), Some(5))).is_empty());
    let fx = step(&mut m, &n, Event::Timeout(n.timeout(3, 1), Some(5)));
    assert_eq!(rounds(&fx), [(2, 2)]);
    assert!(matches!(sends(&fx)[..], [Outbound::SyncInfo(_)]));
    assert_eq!(m.highest_tc().round, 1);
    assert_eq!(
        m.pooled().1,
        0,
        "the timeout pool is cleared with the round"
    );
    assert_eq!(m.record().highest_tc, 1);
}

// ---------------------------------------------------------------- SyncInfo handling (EX-1)

fn sync_info(
    n: &Net,
    qc: xone_primitives::QuorumCert,
    tc: TimeoutCert,
) -> xone_primitives::SyncInfo {
    let _ = n;
    xone_primitives::SyncInfo {
        highest_quorum_cert: qc,
        highest_timeout_cert: tc,
    }
}

fn blank() -> TimeoutCert {
    TimeoutCert {
        round: 0,
        signatures: vec![],
        gap_number: 0,
    }
}

#[test]
fn a_syncinfo_that_does_not_verify_changes_no_state_and_scores_the_peer() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let b2 = n.block(b1, 2, &[0, 1, 2], 0);
    let b3 = n.block(b2, 3, &[0, 1, 2], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    let before = state(&m);
    let info3 = n.chain.info(b3);
    let outsider = Key::new(77);
    let high_s = {
        // the same signature in its malleable high-S form recovers the same signer
        use k256::ecdsa::Signature;
        let good = n.keys[0].sign(&vote_digest(&info3, 0));
        let s = Signature::from_slice(&good[..64]).unwrap();
        let (r, s) = (s.r(), s.s());
        let high = Signature::from_scalars(r, -*s).unwrap();
        let mut b = high.to_bytes().to_vec();
        b.push(good[64] ^ 1);
        Bytes::from(b)
    };
    let mut forged = n.chain.qc(&info3, &n.keys, &[0, 1]);
    forged
        .signatures
        .push(outsider.sign(&vote_digest(&info3, 0)));
    let mut high = n.chain.qc(&info3, &n.keys, &[0, 1, 2]);
    high.signatures[0] = high_s;
    let mut wrong_gap = n.chain.qc(&info3, &n.keys, &[0, 1, 2]);
    wrong_gap.gap_number = 5;
    let mut wrong_info = n.chain.qc(&info3, &n.keys, &[0, 1, 2]);
    wrong_info.proposed_block_info.round = 4;
    let mut bomb = n.chain.qc(&info3, &n.keys, &[0, 1, 2]);
    bomb.signatures = vec![Bytes::from(vec![1; 65]); 151];
    let cases: Vec<(&str, _, Offence)> = vec![
        (
            "forged QC: a signature of a non-member",
            sync_info(&n, forged, blank()),
            Offence::Invalid,
        ),
        (
            "under-threshold QC: 2 of 4",
            sync_info(&n, n.chain.qc(&info3, &n.keys, &[0, 1]), blank()),
            Offence::Invalid,
        ),
        (
            "a QC with no signatures above round 0",
            sync_info(&n, n.chain.qc(&info3, &n.keys, &[]), blank()),
            Offence::Invalid,
        ),
        (
            "a high-S signature",
            sync_info(&n, high, blank()),
            Offence::Invalid,
        ),
        (
            "a gap number that is not the epoch's",
            sync_info(&n, wrong_gap, blank()),
            Offence::Invalid,
        ),
        (
            "block info that is not the header's",
            sync_info(&n, wrong_info, blank()),
            Offence::Invalid,
        ),
        (
            "more signatures than any QC holds",
            sync_info(&n, bomb, blank()),
            Offence::Oversized,
        ),
        (
            "a good QC with an under-threshold TC",
            sync_info(
                &n,
                n.chain.qc(&info3, &n.keys, &[0, 1, 2]),
                n.chain.tc(3, &n.keys, &[1]),
            ),
            Offence::Invalid,
        ),
        (
            "a good QC with a TC of a non-member",
            sync_info(&n, n.chain.qc(&info3, &n.keys, &[0, 1, 2]), {
                let mut t = n.chain.tc(3, &n.keys, &[1, 2]);
                t.signatures
                    .push(outsider.sign(&xone_consensus::v2::cert::timeout_digest(3, 0)));
                t
            }),
            Offence::Invalid,
        ),
    ];
    for (name, si, offence) in cases {
        let fx = step(&mut m, &n, Event::SyncInfo(si, Some(9)));
        assert_eq!(fx, [Effect::ScorePeer { peer: 9, offence }], "{name}");
        assert_eq!(state(&m), before, "{name}: no state changed");
    }
    // a QC of a block the node does not have is not the peer's fault, and changes nothing
    let unknown = BlockInfo {
        hash: B256::repeat_byte(9),
        round: 9,
        number: 109,
    };
    let fx = step(
        &mut m,
        &n,
        Event::SyncInfo(
            sync_info(&n, n.chain.qc(&unknown, &n.keys, &[0, 1, 2]), blank()),
            Some(9),
        ),
    );
    assert!(fx.is_empty());
    assert_eq!(state(&m), before);
    // the good one moves the node to round 4, and the TC of round 5 to round 6
    let tc5 = n.chain.tc(5, &n.keys, &[1, 2, 3]);
    let fx = step(
        &mut m,
        &n,
        Event::SyncInfo(
            sync_info(&n, n.chain.qc(&info3, &n.keys, &[0, 1, 2]), tc5),
            Some(9),
        ),
    );
    assert_eq!(rounds(&fx).iter().map(|r| r.0).collect::<Vec<_>>(), [4, 6]);
    assert_eq!(m.highest_tc().round, 5);
    // the same message again is not newer: dropped without a score
    let again = n.chain.qc(&info3, &n.keys, &[0, 1, 2]);
    assert!(step(
        &mut m,
        &n,
        Event::SyncInfo(
            sync_info(&n, again, n.chain.tc(5, &n.keys, &[1, 2, 3])),
            Some(9)
        )
    )
    .is_empty());
}

// ---------------------------------------------------------------- seeding (xOneGo #718)

#[test]
fn a_validator_that_crosses_the_switch_through_sync_votes_and_times_out() {
    let keys_ = keys(4);
    let mut chain = TestChain::new(keys_.iter().map(Key::address).collect());
    // a V1 head: the machine is not seeded and drops everything
    let v1 = chain.add(99, B256::ZERO, 0, None, 0);
    let mut n = Net {
        keys: keys_,
        chain,
        switch: v1,
    };
    let mut m = n.machine(0);
    for ev in [Event::Synced, Event::Proposal(v1), Event::Timer(1)] {
        assert!(step(&mut m, &n, ev).is_empty());
    }
    assert!(!m.initialized());
    // sync brings the switch block and V2 blocks: the context is derived from them, nothing was seeded by an import
    let s = n.chain.add(100, v1, 0, None, 0);
    n.switch = s;
    let b1 = n.block(s, 1, &[], 0);
    let b2 = n.block(b1, 2, &[0, 1, 2], 0);
    let fx = step(&mut m, &n, Event::Synced);
    assert!(m.initialized());
    assert_eq!(m.round(), 2, "the QC of the head is for round 1");
    assert!(!fx.is_empty());
    let b3 = n.block(b2, 3, &[0, 1, 2], 0);
    let fx = step(&mut m, &n, Event::Proposal(b3));
    assert!(
        fx.windows(3).any(|w| matches!(
            w,
            [
                Effect::PersistIntent { .. },
                Effect::Sign(_),
                Effect::Send(Outbound::Vote { .. })
            ]
        )),
        "vote: {fx:?}"
    );
    let tok = timer(&fx);
    let fx = step(&mut m, &n, Event::Timer(tok));
    assert!(
        sends(&fx)
            .iter()
            .any(|o| matches!(o, Outbound::Timeout { .. })),
        "timeout: {fx:?}"
    );
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Refused { .. })),
        "never ErrV2MasternodesUnavailable: {fx:?}"
    );
}

#[test]
fn a_validator_that_joins_mid_v2_seeds_round_qc_and_lock_from_the_head() {
    let mut n = net(4);
    let mut tip = n.switch;
    for r in 1..=8u64 {
        tip = n.block(tip, r, &[0, 1, 2], 0);
    }
    let mut m = n.machine(2);
    step(&mut m, &n, Event::Synced);
    // the head (round 8) carries the QC of round 7; the QC of its parent (round 6) is the lock
    assert_eq!(m.highest_qc().proposed_block_info.round, 7);
    assert_eq!(m.round(), 8);
    assert_eq!(m.record().lock_qc.map(|l| l.round), Some(6));
    let next = n.block(tip, 9, &[0, 1, 2], 0);
    // the proposal of round 9 carries a QC of round 8: process it, and vote
    let fx = step(&mut m, &n, Event::Proposal(next));
    assert_eq!(sends(&fx).len(), 1, "{fx:?}");
    assert_eq!(m.round(), 9);
}

#[test]
fn a_head_whose_qc_is_the_round_zero_qc_of_the_switch_block_still_seeds_the_highest_qc() {
    // found by the simulator (seed 436): the first V2 block carries the QC of round 0 of the switch block;
    // a machine restarted there kept the placeholder QC and sent SyncInfo that no peer could verify
    let mut n = net(4);
    n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    let mut tok = timer(&step(&mut m, &n, Event::Synced));
    assert_eq!(m.highest_qc().proposed_block_info.hash, n.switch);
    let mut infos = vec![];
    for _ in 0..3 {
        let fx = step(&mut m, &n, Event::Timer(tok));
        tok = timer(&fx);
        infos.extend(sends(&fx).into_iter().filter_map(|o| match o {
            Outbound::SyncInfo(si) => Some(si.highest_quorum_cert.proposed_block_info.hash),
            _ => None,
        }));
    }
    assert_eq!(
        infos,
        [n.switch],
        "the third timeout sends the SyncInfo, and it names the switch block"
    );
}

#[test]
fn a_qc_formed_from_votes_is_stored_and_a_restart_that_gets_it_back_is_not_left_behind() {
    // found by the simulator (seed 806): the QC lives only in the node that formed it; the node timed out in
    // the next round (so its record is at round 2), crashed and came back in round 1, where its record
    // refuses a timeout, and the others needed that timeout
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    let mut tok = timer(&step(&mut m, &n, Event::Synced));
    step(&mut m, &n, Event::Proposal(b1));
    let info = n.chain.info(b1);
    let mut stored = None;
    for who in [1, 2, 3] {
        let fx = step(&mut m, &n, Event::Vote(n.vote(who, &info), Some(1)));
        for e in &fx {
            if let Effect::Store(si) = e {
                stored = Some(si.clone());
            }
            if let Effect::SetTimer { token, .. } = e {
                tok = *token;
            }
        }
    }
    let stored = stored.expect("the QC of round 1 is handed to the driver");
    assert_eq!(stored.highest_quorum_cert.proposed_block_info, info);
    step(&mut m, &n, Event::Timer(tok));
    assert_eq!(m.record().highest_voted_round, 2, "timed out in round 2");
    // the restart: the record is back, the QC is not
    let mut r = Bft::new(
        Config {
            v2: v2(&[(0, row(10, 3, 2))]),
            gap: 450,
            me: Some(n.keys[0].address()),
        },
        Secp,
        m.record().clone(),
    )
    .unwrap();
    let tok = timer(&step(&mut r, &n, Event::Synced));
    assert_eq!(
        r.round(),
        1,
        "the head carries the QC of the switch block only"
    );
    let fx = step(&mut r, &n, Event::Timer(tok));
    assert!(
        matches!(
            &fx[..],
            [
                Effect::Refused {
                    kind: Kind::Timeout,
                    ..
                },
                Effect::SetTimer { .. }
            ]
        ),
        "{fx:?}"
    );
    // the stored SyncInfo comes back as a SyncInfo of the node to itself
    let fx = step(&mut r, &n, Event::SyncInfo(stored, None));
    assert_eq!(rounds(&fx), [(2, 2)]);
    let tok = timer(&fx);
    let fx = step(&mut r, &n, Event::Timer(tok));
    assert!(
        sends(&fx)
            .iter()
            .any(|o| matches!(o, Outbound::Timeout { round: 2, .. })),
        "{fx:?}"
    );
}

// ---------------------------------------------------------------- the mine period (xOneGo #721)

#[test]
fn the_mine_period_of_the_config_drives_the_round_effect() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    // a fast devnet: 1 second, then 5 seconds from round 3
    let rows = [(0, row(10, 3, 1)), (3, row(10, 3, 5))];
    let mut m = n.machine_with(0, &rows, Secp);
    assert_eq!(rounds(&step(&mut m, &n, Event::Synced)), [(1, 1)]);
    step(&mut m, &n, Event::Proposal(b1));
    let info1 = n.chain.info(b1);
    let mut fx = vec![];
    for who in [0, 1, 2] {
        fx = step(&mut m, &n, Event::Vote(n.vote(who, &info1), Some(1)));
    }
    assert_eq!(rounds(&fx), [(2, 1)]);
    let b2 = n.block(b1, 2, &[0, 1, 2], 0);
    step(&mut m, &n, Event::Proposal(b2));
    let info2 = n.chain.info(b2);
    for who in [0, 1, 2] {
        fx = step(&mut m, &n, Event::Vote(n.vote(who, &info2), Some(1)));
    }
    assert_eq!(rounds(&fx), [(3, 5)]);
}

// ---------------------------------------------------------------- the sibling vote

#[test]
fn a_same_height_sibling_after_a_timeout_is_voted_when_its_round_starts() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    step(&mut m, &n, Event::Proposal(b1));
    let i1 = n.chain.info(b1);
    let mut tok = 0;
    for who in [0, 1, 2] {
        let fx = step(&mut m, &n, Event::Vote(n.vote(who, &i1), Some(1)));
        if fx.iter().any(|e| matches!(e, Effect::SetTimer { .. })) {
            tok = timer(&fx);
        }
    }
    assert_eq!(m.round(), 2);
    let b2 = n.block(b1, 2, &[0, 1, 2], 0); // N at height 102, round 2
    let fx = step(&mut m, &n, Event::Proposal(b2));
    assert_eq!(sends(&fx).len(), 1, "N is voted in round 2");
    // round 2 times out
    let fx = step(&mut m, &n, Event::Timer(tok));
    assert!(
        sends(&fx)
            .iter()
            .any(|o| matches!(o, Outbound::Timeout { round: 2, .. })),
        "{fx:?}"
    );
    // N' at the same height, same parent, round 3, executed (not the head) before the TC of round 2
    let b2s = n.block(b1, 3, &[0, 1, 2], 7);
    assert_eq!(n.chain.headers[&b2s].number, n.chain.headers[&b2].number);
    assert!(
        step(&mut m, &n, Event::Proposal(b2s)).is_empty(),
        "the TC has not come: the sibling waits"
    );
    // the TC of round 2
    let mut fx = vec![];
    for who in [1, 2, 3] {
        fx = step(&mut m, &n, Event::Timeout(n.timeout(who, 2), Some(1)));
    }
    assert_eq!(m.round(), 3);
    assert!(
        fx.windows(3).any(|w| matches!(w, [Effect::PersistIntent { intent, .. }, Effect::Sign(_), Effect::Send(Outbound::Vote { info, .. })]
            if intent.kind == Kind::Vote && intent.round == 3 && info.hash == b2s)),
        "the sibling is voted right after the round starts: {fx:?}"
    );
}

#[test]
fn the_lock_qc_refuses_a_proposal_that_does_not_extend_it() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let b2 = n.block(b1, 2, &[0, 1, 2], 0);
    let b3 = n.block(b2, 3, &[0, 1, 2], 0);
    // a fork off the switch block, with an old QC
    let f1 = n.block(n.switch, 4, &[], 5);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    // b3's QC certifies b2; the QC inside b2 certifies b1: the lock is b1 (round 1)
    step(&mut m, &n, Event::Proposal(b3));
    assert_eq!(m.record().lock_qc.map(|l| l.round), Some(1));
    // the round moves on to 4 by a TC; f1 is in round 4 with a QC of round 0, not above the lock, and does not extend b1
    let mut fx = vec![];
    for who in [1, 2, 3] {
        fx = step(&mut m, &n, Event::Timeout(n.timeout(who, 3), Some(1)));
    }
    assert_eq!(m.round(), 4, "{fx:?}");
    assert!(
        step(&mut m, &n, Event::Proposal(f1)).is_empty(),
        "no vote for a block that does not extend the lock"
    );
    let _ = Intent::new(Kind::Vote, 1, B256::ZERO);
    let _ = Error::NoMasternodes;
}

// ---------------------------------------------------------------- floods (CertiK P3, XDC-07)

/// A stand-in scheme in which one signer has any number of distinct valid signatures of one message,
/// which real ECDSA also allows (the nonce is free): `signer(20) | nonce(8)`.
struct Variants(Vec<Address>);

impl Scheme for Variants {
    fn recover(&self, _: &B256, sig: &[u8]) -> Result<Address, Error> {
        let a = sig.get(..20).ok_or(Error::SignatureUnrecoverable)?;
        let a = Address::from_slice(a);
        self.0
            .contains(&a)
            .then_some(a)
            .ok_or(Error::SignatureUnrecoverable)
    }
}

#[test]
fn ten_thousand_variants_of_one_signer_keep_one_entry_and_cost_bounded_time() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let who = n.keys[1].address();
    let mut m = n.machine_with(
        0,
        &[(0, row(10, 3, 2))],
        Variants(n.chain.masternodes.clone()),
    );
    step(&mut m, &n, Event::Synced);
    let info = n.chain.info(b1);
    let t = Instant::now();
    let mut worst = std::time::Duration::ZERO;
    let mut total_effects = 0;
    for i in 0..10_000u64 {
        let mut sig = who.to_vec();
        sig.extend_from_slice(&i.to_be_bytes());
        // votes for one block, votes for other blocks, and timeouts, all from one signer and all valid
        let other = BlockInfo {
            hash: B256::with_last_byte((i % 251) as u8),
            ..info.clone()
        };
        let v = Vote {
            proposed_block_info: if i % 3 == 0 { info.clone() } else { other },
            signature: sig.clone().into(),
            gap_number: 0,
        };
        let to = Timeout {
            round: 1,
            signature: sig.into(),
            gap_number: 0,
        };
        for ev in [Event::Vote(v, Some(2)), Event::Timeout(to, Some(2))] {
            let t0 = Instant::now();
            total_effects += step(&mut m, &n, ev).len();
            worst = worst.max(t0.elapsed());
        }
    }
    let per = t.elapsed() / 20_000;
    eprintln!("20,000 variants: pooled {:?}, {total_effects} effects, {per:?} per message, worst {worst:?}", m.pooled());
    assert_eq!(m.pooled(), (1, 1), "one vote and one timeout of the signer");
    assert_eq!(
        total_effects, 0,
        "no certificate, no round change, no score"
    );
    assert!(m.round() == 1);
    // the same signer cannot be counted twice into a QC
    assert!(per < std::time::Duration::from_millis(1));
}

#[test]
fn real_signature_variants_that_do_not_verify_cost_a_score_each_and_no_memory() {
    let mut n = net(4);
    let b1 = n.block(n.switch, 1, &[], 0);
    let mut m = n.machine(0);
    step(&mut m, &n, Event::Synced);
    let info = n.chain.info(b1);
    let good = n.keys[1].sign(&vote_digest(&info, 0));
    let mut scored = 0;
    for i in 0..2_000u32 {
        // flip bits of a real signature: most do not recover a member, none is kept
        let mut s = good.to_vec();
        s[(i % 64) as usize] ^= 1 << (i % 8);
        let v = Vote {
            proposed_block_info: info.clone(),
            signature: s.into(),
            gap_number: 0,
        };
        scored += step(&mut m, &n, Event::Vote(v, Some(2))).len();
    }
    assert!(scored > 1_900);
    assert!(
        m.pooled().0 <= 1,
        "at most the one variant that still recovers a member"
    );
}
