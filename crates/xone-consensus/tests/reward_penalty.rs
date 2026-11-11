#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: fixtures are trusted inputs and a panic is the failure signal"
)]
//! Engine tests on synthetic chains. The tests whose names start with `mutant_` are the ones
//! `tests/reward_mutants.rs` expects to fail when the matching rule is flipped in the source.

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_primitives::{keccak256, Address, B256, U256};
use xone_chainspec::{apothem, ChainSpec, ForkBlock};
use xone_consensus::chain::{
    BlockReader, Error, HeaderView, PenaltyHistory, SignTx, SignTxCounter, SignedBlock,
};
use xone_consensus::penalty::{v1 as pv1, v2 as pv2};
use xone_consensus::reward::{self, proportional, tiered, v1, v2, Holders, RewardWindow, Rule};

fn a(i: u8) -> Address {
    Address::repeat_byte(i)
}

fn hash(n: u64, fork: u8) -> B256 {
    keccak256([n.to_be_bytes().as_slice(), &[fork]].concat())
}

/// A chain by hash: headers, optional bodies, and the previous epochs' penalty lists.
#[derive(Default)]
struct Chain {
    headers: BTreeMap<B256, HeaderView>,
    bodies: BTreeMap<B256, Vec<SignTx>>,
    history: Vec<Vec<Address>>,
}

impl Chain {
    /// Blocks `from..=to` on branch `fork`; `switch` marks the epoch-switch blocks.
    fn extend(
        &mut self,
        from: u64,
        to: u64,
        fork: u8,
        switch: impl Fn(u64) -> bool,
        miner: impl Fn(u64) -> Address,
    ) {
        for n in from..=to {
            let hv = HeaderView {
                number: n,
                hash: hash(n, fork),
                parent_hash: hash(n.saturating_sub(1), fork),
                miner: miner(n),
                epoch_switch: switch(n),
            };
            self.headers.insert(hv.hash, hv);
            self.bodies.insert(hv.hash, Vec::new());
        }
    }

    fn sign(&mut self, in_block: u64, signer: Address, signed_block: u64) {
        self.bodies
            .get_mut(&hash(in_block, 0))
            .unwrap()
            .push(SignTx {
                sender: signer,
                signed: hash(signed_block, 0),
                ok: true,
            });
    }
}

impl BlockReader for Chain {
    fn header(&self, h: B256, _: u64) -> Option<HeaderView> {
        self.headers.get(&h).copied()
    }
    fn sign_txs(&self, h: B256, _: u64) -> Option<Vec<SignTx>> {
        self.bodies.get(&h).cloned()
    }
}

impl PenaltyHistory for Chain {
    fn penalties(&self, _: B256, back: usize) -> Option<Vec<Address>> {
        self.history.get(back).cloned()
    }
}

/// A reader that finds a block by its height only, as a store keyed by number does after a reorg.
struct ByNumber(BTreeMap<u64, HeaderView>);

impl BlockReader for ByNumber {
    fn header(&self, _: B256, number: u64) -> Option<HeaderView> {
        self.0.get(&number).copied()
    }
    fn sign_txs(&self, _: B256, _: u64) -> Option<Vec<SignTx>> {
        Some(Vec::new())
    }
}

// ---- counting and payout ----

/// Blocks 1..=n with signers of block k sent in block k+1.
fn window(
    signs: &[(u64, Address)],
    start: u64,
    end: u64,
    last: u64,
    masternodes: &[Address],
) -> RewardWindow {
    let blocks = (start..=last)
        .map(|n| SignedBlock {
            number: n,
            hash: hash(n, 0),
            signs: signs
                .iter()
                .filter(|(k, _)| *k + 1 == n)
                .map(|&(k, s)| SignTx {
                    sender: s,
                    signed: hash(k, 0),
                    ok: true,
                })
                .collect(),
        })
        .collect();
    RewardWindow::new(start, end, blocks, masternodes.to_vec(), vec![], vec![]).unwrap()
}

#[test]
fn flat_count_keeps_multiples_of_15_masternodes_and_one_per_block() {
    let spec = apothem();
    let w = window(
        &[
            (15, a(1)),
            (15, a(1)),
            (15, a(9)),
            (16, a(1)),
            (30, a(1)),
            (30, a(2)),
        ],
        1,
        60,
        61,
        &[a(1), a(2)],
    );
    let c = w.flat_counts(&spec, false);
    // Block 15: a(1) twice is once, a(9) is no masternode; block 16 is not a multiple; block 30: both.
    assert_eq!((c.total, c.signs[&a(1)], c.signs[&a(2)]), (3, 2, 1));
}

#[test]
fn flat_payout_divides_before_it_multiplies() {
    let c = reward::Counts {
        total: 7,
        signs: [(a(1), 5), (a(2), 2)].into(),
    };
    let p = v1::payout(U256::from(100u64), &c);
    // 100 / 7 = 14 first: 70 and 28, not 71 and 28 (100 * 5 / 7).
    assert_eq!((p[&a(1)], p[&a(2)]), (U256::from(70u64), U256::from(28u64)));
    assert!(v1::payout(U256::from(100u64), &reward::Counts::default()).is_empty());
}

#[test]
fn proportional_payout_caps_at_the_slots_and_is_zero_without_slots() {
    let s: BTreeMap<Address, u64> = [(a(1), 7), (a(2), 3)].into();
    let p = proportional::payout(U256::from(1000u64), &s, 5);
    assert_eq!(
        (p[&a(1)], p[&a(2)]),
        (U256::from(1000u64), U256::from(600u64))
    );
    assert_eq!(
        proportional::payout(U256::from(1000u64), &s, 0)[&a(1)],
        U256::ZERO
    );
    assert_eq!(tiered::fixed(U256::from(9u64), &s)[&a(2)], U256::from(9u64));
}

#[test]
fn tier_counts_slots_are_every_15th_block_even_unsigned() {
    let w = RewardWindow::new(
        1,
        60,
        (1..=61)
            .map(|n| SignedBlock {
                number: n,
                hash: hash(n, 0),
                signs: vec![],
            })
            .collect(),
        vec![a(1)],
        vec![a(2)],
        vec![a(3)],
    )
    .unwrap();
    let t = w.tier_counts();
    assert_eq!(t.slots, 4);
    assert!(t.masternode.is_empty());
}

#[test]
fn tiers_split_the_standby_pool() {
    let spec = apothem();
    let cfg = tiered::config(&spec, 27_400_600).unwrap();
    let pool: Vec<Address> = (1..=10).map(a).collect();
    let (p, o) = tiered::tiers(cfg, &pool);
    assert_eq!((p.len(), o.len()), (10, 0));
    let mut cfg = cfg.clone();
    cfg.max_protector_nodes = 3;
    cfg.max_observer_nodes = 2;
    let (p, o) = tiered::tiers(&cfg, &pool);
    assert_eq!((p, o), (pool[..3].to_vec(), pool[3..5].to_vec()));
}

// ---- holders ----

fn holders(root: B256, owner: Address) -> Holders {
    Holders::new(
        root,
        [(
            a(1),
            owner,
            vec![(a(7), U256::from(5u64)), (a(7), U256::from(5u64))],
        )],
    )
}

fn one_signer_window() -> RewardWindow {
    let spec = apothem();
    let r = v1::checkpoint_period(&spec);
    let n = 5 * r;
    let (start, end) = (n - 2 * r + 1, n - r);
    let signs: Vec<(u64, Address)> = (start..=end)
        .filter(|k| k % 15 == 0)
        .map(|k| (k, a(1)))
        .collect();
    window(&signs, start, end, n - 1, &[a(1)])
}

#[test]
fn v1_flat_pays_90_10_and_touches_the_voters() {
    let spec = apothem();
    let root = hash(1, 9);
    let w = one_signer_window();
    let n = 5 * v1::checkpoint_period(&spec);
    let m = v1::rewards(&spec, n, &w, &holders(root, a(2)), root).unwrap();
    // 60 blocks of the window are multiples of 15; one signer signed all of them, so it gets
    // (chain / 60) * 60, which is 18 wei short of the chain reward: 90% to the owner, 10% to the
    // foundation, 0 to the voter.
    let paid = U256::from(5000u64) * U256::from(10u64).pow(U256::from(18u64)) / U256::from(60u64)
        * U256::from(60u64);
    assert_eq!(m[&a(2)], paid * U256::from(90u64) / U256::from(100u64));
    assert_eq!(m[&spec.xdpos.foundation_wallet], paid / U256::from(10u64));
    assert_eq!(m[&a(7)], U256::ZERO);
}

#[test]
fn v1_owner_that_is_the_foundation_gets_no_owner_share_but_tiered_does() {
    let spec = apothem();
    let root = hash(1, 9);
    let f = spec.xdpos.foundation_wallet;
    let w = one_signer_window();
    let n = 5 * v1::checkpoint_period(&spec);
    let paid = U256::from(5000u64) * U256::from(10u64).pow(U256::from(18u64)) / U256::from(60u64)
        * U256::from(60u64);
    let m = v1::rewards(&spec, n, &w, &holders(root, f), root).unwrap();
    assert_eq!(m[&f], paid / U256::from(10u64));
    // The tiered path always credits the owner share (reward.go:333).
    let mut spec = spec;
    spec.tip_proportional_reward = ForkBlock::At(0);
    let cfg = tiered::config(&spec, 27_400_600).unwrap();
    let w = RewardWindow::new(
        w.start(),
        w.end(),
        (w.start()..n)
            .map(|k| SignedBlock {
                number: k,
                hash: hash(k, 0),
                signs: if k % 15 == 1 {
                    vec![SignTx {
                        sender: a(1),
                        signed: hash(k - 1, 0),
                        ok: true,
                    }]
                } else {
                    vec![]
                },
            })
            .collect(),
        vec![a(1)],
        vec![],
        vec![],
    )
    .unwrap();
    let t = tiered::rewards(&spec, n, cfg, &w, &holders(root, f), root).unwrap();
    let paid = reward::tier_reward(cfg.masternode_reward).unwrap();
    assert_eq!(
        t[&f],
        paid * U256::from(90u64) / U256::from(100u64) + paid / U256::from(10u64)
    );
}

#[test]
fn mutant_parent_state_root() {
    let spec = apothem();
    let w = one_signer_window();
    let n = 5 * v1::checkpoint_period(&spec);
    let (parent, post) = (hash(1, 9), hash(2, 9));
    // The owner changed in a transaction of the checkpoint block: the post-state says a(3).
    let seen_post = holders(post, a(3));
    assert_eq!(
        v1::rewards(&spec, n, &w, &seen_post, parent),
        Err(Error::WrongState {
            want: parent,
            got: post
        })
    );
    assert!(v1::rewards(&spec, n, &w, &holders(parent, a(2)), parent).is_ok());
}

#[test]
fn a_masternode_without_a_holder_entry_is_an_error_not_a_zero_owner() {
    let spec = apothem();
    let w = one_signer_window();
    let n = 5 * v1::checkpoint_period(&spec);
    let root = hash(1, 9);
    let empty = Holders::new(root, []);
    assert_eq!(
        v1::rewards(&spec, n, &w, &empty, root),
        Err(Error::NoHolder(a(1)))
    );
}

#[test]
fn rule_follows_the_fork_rows() {
    let s = apothem();
    assert_eq!(reward::rule(&s, 56_000_000), Rule::V1);
    assert_eq!(reward::rule(&s, 83_599_999), Rule::V2);
    assert_eq!(
        reward::rule(&s, 83_600_000),
        Rule::Tiered {
            proportional: false
        }
    );
    assert_eq!(
        reward::rule(&s, 84_732_999),
        Rule::Tiered {
            proportional: false
        }
    );
    assert_eq!(
        reward::rule(&s, 84_733_000),
        Rule::Tiered { proportional: true }
    );
}

#[test]
fn inflation_halves_only_before_no_halving() {
    let mut s = apothem();
    let r = U256::from(1000u64);
    // Apothem: no halving from 23,779,191.
    assert_eq!(reward::inflation(&s, 31_536_000, r), r);
    s.effective.tip_no_halving_mn_reward = ForkBlock::Never;
    assert_eq!(reward::inflation(&s, 31_535_999, r), r);
    assert_eq!(reward::inflation(&s, 31_536_000, r), U256::from(500u64));
    assert_eq!(reward::inflation(&s, 78_840_000, r), U256::from(250u64));
}

// ---- the window and the counter ----

#[test]
fn v2_locate_skips_the_switch_plus_one_and_finds_the_second_switch() {
    let mut s = apothem();
    s.xdpos.v2.as_mut().unwrap().switch_block = 100;
    let mut c = Chain::default();
    // Epoch switches at 101 (the V2 switch + 1, not a switch for the walk), 200 and 300.
    c.extend(0, 400, 0, |n| [101, 200, 300].contains(&n), |_| a(1));
    let at = v2::locate(&s, &c, 400, hash(399, 0)).unwrap();
    assert_eq!((at.start, at.end, at.switch.1), (201, 299, 200));
    // Below that there is no second switch except 101, which does not count.
    assert_eq!(
        v2::locate(&s, &c, 250, hash(249, 0)),
        Err(Error::WindowNotFound(250))
    );
    // A missing header is a typed error.
    c.headers.remove(&hash(350, 0));
    assert!(matches!(
        v2::locate(&s, &c, 400, hash(399, 0)),
        Err(Error::Missing {
            what: "header",
            number: 350,
            ..
        })
    ));
}

#[test]
fn counter_collects_ascending_along_the_parent_chain() {
    let mut c = Chain::default();
    c.extend(0, 40, 0, |_| false, |_| a(1));
    c.sign(31, a(2), 30);
    let got = SignTxCounter::new(&c)
        .collect((hash(40, 0), 40), 30)
        .unwrap();
    assert_eq!(
        got.iter().map(|b| b.number).collect::<Vec<_>>(),
        (30..=40).collect::<Vec<_>>()
    );
    assert_eq!(got[1].signs[0].sender, a(2));
}

#[test]
fn mutant_missing_body_is_not_zero_signatures() {
    let mut c = Chain::default();
    c.extend(0, 40, 0, |_| false, |_| a(1));
    c.bodies.remove(&hash(35, 0));
    assert_eq!(
        SignTxCounter::new(&c).collect((hash(40, 0), 40), 30),
        Err(Error::Missing {
            what: "body",
            number: 35,
            hash: hash(35, 0)
        })
    );
}

#[test]
fn mutant_side_block_is_not_counted() {
    // Block 35 of a side branch replaced the canonical one in a store keyed by height.
    let mut c = Chain::default();
    c.extend(0, 40, 0, |_| false, |_| a(1));
    let mut by_number: BTreeMap<u64, HeaderView> =
        c.headers.values().map(|h| (h.number, *h)).collect();
    by_number.insert(
        35,
        HeaderView {
            hash: hash(35, 7),
            parent_hash: hash(34, 7),
            ..by_number[&35]
        },
    );
    let got = SignTxCounter::new(&ByNumber(by_number)).collect((hash(40, 0), 40), 30);
    assert_eq!(
        got,
        Err(Error::NotCanonical {
            number: 35,
            want: hash(35, 0),
            got: hash(35, 7)
        })
    );
}

// ---- penalties ----

/// Epoch 30, V2 switch at 10, the epoch switch at 60 and the one being sealed at 90.
fn pspec(upgraded: bool) -> ChainSpec {
    let mut s = apothem();
    s.xdpos.epoch = 30;
    let v2 = s.xdpos.v2.as_mut().unwrap();
    v2.switch_block = 10;
    for c in v2.configs.values_mut() {
        c.minimum_miner_block_per_epoch = 1;
        c.limit_penalty_epoch = 2;
        c.minimum_signing_tx = 2;
    }
    s.tip_upgrade_penalty = if upgraded {
        ForkBlock::At(0)
    } else {
        ForkBlock::At(99_999_999_999)
    };
    s.parole_dedup = ForkBlock::Never;
    s
}

fn epoch_chain(history: Vec<Vec<Address>>) -> Chain {
    let mut c = Chain {
        history,
        ..Chain::default()
    };
    // a(1) and a(2) seal alternately; a(3), penalised before, seals nothing.
    c.extend(
        0,
        89,
        0,
        |n| n == 60 || n == 30 || n == 0,
        |n| a(1 + (n % 2) as u8),
    );
    c
}

fn input<'a>(masternodes: &'a [Address], candidates: &'a [Address]) -> pv2::Input<'a> {
    pv2::Input {
        number: 90,
        parent: hash(89, 0),
        round: 27_400_600,
        parent_masternodes: masternodes,
        candidates,
    }
}

#[test]
fn v2_penalises_masternodes_that_mined_nothing_and_not_at_switch_plus_one() {
    let s = pspec(false);
    let c = epoch_chain(vec![vec![]]);
    let p = pv2::penalties(&s, &c, &input(&[a(1), a(2), a(4)], &[a(1), a(2), a(4)])).unwrap();
    assert_eq!(p, vec![a(4)]);
    let one = [a(1)];
    let mut at_one = input(&one, &one);
    at_one.number = 11;
    assert_eq!(
        pv2::penalties(&s, &c, &at_one).unwrap(),
        Vec::<Address>::new()
    );
    at_one.number = 10;
    assert_eq!(pv2::penalties(&s, &c, &at_one), Err(Error::NotV2(10)));
}

#[test]
fn v2_minimum_blocks_come_from_the_config_after_the_upgrade() {
    let mut s = pspec(true);
    s.xdpos
        .v2
        .as_mut()
        .unwrap()
        .configs
        .values_mut()
        .for_each(|c| c.minimum_miner_block_per_epoch = 16);
    let c = epoch_chain(vec![vec![], vec![]]);
    // a(1) and a(2) mined 15 each in 61..=89 (29 blocks: 15 and 14), below 16: both penalised.
    let p = pv2::penalties(&s, &c, &input(&[a(1), a(2)], &[a(1), a(2)])).unwrap();
    assert_eq!(p.len(), 2);
}

#[test]
fn v2_pre_upgrade_comeback_needs_a_sign_for_a_15th_block() {
    let s = pspec(false);
    let mut c = epoch_chain(vec![vec![a(3)]]);
    let nodes = [a(1), a(2)];
    let cands = [a(1), a(2), a(3)];
    assert_eq!(
        pv2::penalties(&s, &c, &input(&nodes, &cands)).unwrap(),
        vec![a(3)]
    );
    // a(3) signs block 75 (a multiple of 15) in block 80: released.
    c.sign(80, a(3), 75);
    assert!(pv2::penalties(&s, &c, &input(&nodes, &cands))
        .unwrap()
        .is_empty());
}

#[test]
fn mutant_out_of_order_scan_loses_a_sign_in_the_flagging_block() {
    // The signature sits in block 75, which is the block it signs: it counts only if the scan has
    // flagged block 75 before it reads the body of 75.
    let s = pspec(false);
    let mut c = epoch_chain(vec![vec![a(3)]]);
    c.sign(76, a(3), 75);
    c.sign(75, a(3), 75);
    let nodes = [a(1), a(2)];
    assert!(pv2::penalties(&s, &c, &input(&nodes, &[a(1), a(2), a(3)]))
        .unwrap()
        .is_empty());
    // And a signature in an earlier block for a later one never counts.
    let mut c = epoch_chain(vec![vec![a(3)]]);
    c.sign(70, a(3), 75);
    assert_eq!(
        pv2::penalties(&s, &c, &input(&nodes, &[a(1), a(2), a(3)])).unwrap(),
        vec![a(3)]
    );
}

#[test]
fn v2_comeback_height_bound_and_non_candidates() {
    let s = pspec(false);
    let c = epoch_chain(vec![vec![a(3), a(9)]]);
    let nodes = [a(1), a(2)];
    // a(9) was penalised but is no candidate any more: not carried over.
    assert_eq!(
        pv2::penalties(&s, &c, &input(&nodes, &[a(1), a(2), a(3)])).unwrap(),
        vec![a(3)]
    );
}

#[test]
fn mutant_limit_penalty_epoch_of_the_pre_upgrade_rule() {
    // Listed in the newest epoch only, with a limit of 2: still penalised. With the pre-upgrade
    // limit of 0 the loop never runs and it would be released.
    let s = pspec(true);
    let c = epoch_chain(vec![vec![a(3)], vec![]]);
    let p = pv2::penalties(&s, &c, &input(&[a(1), a(2)], &[a(1), a(2), a(3)])).unwrap();
    assert_eq!(p, vec![a(3)]);
    assert_eq!(pv2::post_upgrade_limit(0), 1);
    assert_eq!(pv2::post_upgrade_limit(5), 5);
}

#[test]
fn v2_post_upgrade_release_needs_the_limit_and_enough_signs() {
    let s = pspec(true);
    let nodes = [a(1), a(2)];
    let cands = [a(1), a(2), a(3)];
    let mut c = epoch_chain(vec![vec![a(3)], vec![a(3)]]);
    // Two epochs listed, one sign: still penalised.
    c.sign(80, a(3), 75);
    assert_eq!(
        pv2::penalties(&s, &c, &input(&nodes, &cands)).unwrap(),
        vec![a(3)]
    );
    // Two signs for two multiples of 15: released.
    c.sign(81, a(3), 60);
    assert!(pv2::penalties(&s, &c, &input(&nodes, &cands))
        .unwrap()
        .is_empty());
}

#[test]
fn v2_parole_dedup_counts_a_pair_once() {
    let mut s = pspec(true);
    let mut c = epoch_chain(vec![vec![a(3)], vec![a(3)]]);
    c.sign(80, a(3), 75);
    c.sign(81, a(3), 75);
    let nodes = [a(1), a(2)];
    let cands = [a(1), a(2), a(3)];
    assert!(pv2::penalties(&s, &c, &input(&nodes, &cands))
        .unwrap()
        .is_empty());
    s.parole_dedup = ForkBlock::At(0);
    assert_eq!(
        pv2::penalties(&s, &c, &input(&nodes, &cands)).unwrap(),
        vec![a(3)]
    );
}

#[test]
fn mutant_penalty_bounds_fail_open() {
    let s = pspec(false);
    let nodes = [a(1), a(2)];
    let cands = [a(1), a(2), a(3)];
    // The previous epoch's penalties are not available: verification must fail, not pass.
    let c = epoch_chain(vec![]);
    let r = pv2::verify(&s, &c, &input(&nodes, &cands), &[a(3)]);
    assert!(
        matches!(
            r,
            Err(Error::Missing {
                what: "epoch-switch penalties",
                ..
            })
        ),
        "{r:?}"
    );
    // With history, equal sets pass in any order, a different set is a typed mismatch.
    let c = epoch_chain(vec![vec![a(3)]]);
    assert!(pv2::verify(&s, &c, &input(&nodes, &cands), &[a(3)]).is_ok());
    assert!(matches!(
        pv2::verify(&s, &c, &input(&nodes, &cands), &[]),
        Err(Error::PenaltyMismatch { .. })
    ));
}

#[test]
fn mutant_switch_plus_one_special_case() {
    // Nothing is penalised at the V2 switch block plus one; a header that says otherwise is rejected.
    let s = pspec(false);
    let c = epoch_chain(vec![vec![]]);
    let one = [a(1)];
    let mut at = input(&one, &one);
    at.number = 11;
    assert!(pv2::verify(&s, &c, &at, &[]).is_ok());
    assert!(matches!(
        pv2::verify(&s, &c, &at, &[a(5)]),
        Err(Error::PenaltyMismatch { .. })
    ));
}

#[test]
fn v1_penalties_and_comeback() {
    let mut s = apothem();
    s.xdpos.epoch = 30;
    s.effective.tip_randomize = ForkBlock::At(0);
    let number = 30 * 6;
    let mut c = Chain::default();
    c.extend(0, number - 1, 0, |_| false, |n| a(1 + (n % 2) as u8));
    let comeback = pv1::comeback_epoch(&s, number).unwrap();
    assert_eq!(comeback, number - 5 * 30);
    let mut inp = pv1::Input {
        number,
        parent: hash(number - 1, 0),
        candidates: &[a(1), a(2), a(3)],
        prev_masternodes: &[a(1), a(2), a(4)],
        comeback_penalties: Some(&[a(3)]),
    };
    // a(4) sealed nothing; a(3) was penalised five epochs ago and has not signed.
    let p = pv1::penalties(&s, &c, &inp).unwrap();
    assert_eq!(p, vec![a(4), a(3)]);
    // Before TIPRandomize only the comeback nodes are listed.
    s.effective.tip_randomize = ForkBlock::Never;
    assert_eq!(pv1::penalties(&s, &c, &inp).unwrap(), vec![a(3)]);
    // The header of the comeback epoch is required.
    inp.comeback_penalties = None;
    assert!(matches!(
        pv1::penalties(&s, &c, &inp),
        Err(Error::Missing { .. })
    ));
    // Below one epoch there is nothing to penalise.
    inp.number = 29;
    assert!(pv1::penalties(&s, &c, &inp).unwrap().is_empty());
}

// ---- immutability (xOneGo#700) ----

#[test]
fn the_window_is_shared_read_only_across_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<RewardWindow>();
    send_sync::<Holders>();
    let spec = Arc::new(apothem());
    let w = Arc::new(one_signer_window());
    let root = hash(1, 9);
    let h = Arc::new(holders(root, a(2)));
    let n = 5 * v1::checkpoint_period(&spec);
    let want = v1::rewards(&spec, n, &w, &h, root).unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (spec, w, h) = (spec.clone(), w.clone(), h.clone());
            std::thread::spawn(move || {
                (0..200)
                    .map(|_| v1::rewards(&spec, n, &w, &h, root).unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for t in handles {
        assert!(t.join().unwrap().iter().all(|m| *m == want));
    }
    // Nothing that is reachable from a shared reference can change the window.
    assert_eq!(w.flat_counts(&spec, false), w.flat_counts(&spec, false));
}

// ---- start-up: a missing fork value ----

#[test]
fn a_missing_reward_or_penalty_fork_value_stops_loading() {
    for key in [
        "tipFixedRewardBlock",
        "tipUpgradePenaltyBlock",
        "tipProportionalRewardBlock",
        "gas2500xBlock",
    ] {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("../../xone-chainspec/generated/forks.json"))
                .unwrap();
        v["networks"]["apothem"]["blocks"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        let r = xone_chainspec::load(xone_chainspec::Network::Apothem, &v.to_string());
        assert!(
            matches!(r, Err(xone_chainspec::Error::MissingFork(k)) if k == key),
            "{key}: {r:?}"
        );
    }
}

// ---- wide accounts ----

#[test]
fn a_reward_credits_an_account_above_2_pow_256() {
    use xone_primitives::WideBalance;
    let spec = apothem();
    let root = hash(1, 9);
    let w = one_signer_window();
    let n = 5 * v1::checkpoint_period(&spec);
    // The owner of the signer is a genesis-funded account with 2^256 - 1.
    let m = v1::rewards(&spec, n, &w, &holders(root, a(2)), root).unwrap();
    let before = WideBalance::from(U256::MAX);
    let after = before.checked_add(m[&a(2)]).unwrap();
    assert!(
        after.is_wide(),
        "the sum is a true integer above 2^256 - 1, not wrapped"
    );
    assert_eq!(after.low(), m[&a(2)] - U256::from(1u64));
}

// ---- inputs ----

#[test]
fn a_sign_transaction_is_exactly_68_bytes_of_sign_to_0x89() {
    use xone_consensus::chain::{signed_hash, BLOCK_SIGNERS};
    let mut data = vec![0xe3, 0x41, 0xea, 0xa4];
    data.extend([0u8; 32]);
    data.extend(hash(5, 0).0);
    assert_eq!(signed_hash(Some(BLOCK_SIGNERS), &data), Some(hash(5, 0)));
    assert_eq!(signed_hash(Some(a(1)), &data), None);
    assert_eq!(signed_hash(None, &data), None);
    assert_eq!(signed_hash(Some(BLOCK_SIGNERS), &data[..67]), None);
    data.push(0);
    assert_eq!(signed_hash(Some(BLOCK_SIGNERS), &data), None);
    data.pop();
    data[0] = 0;
    assert_eq!(signed_hash(Some(BLOCK_SIGNERS), &data), None);
}

#[test]
fn holders_are_read_from_the_validator_contract_slots() {
    use alloy_primitives::{keccak256, U256};
    use std::collections::HashMap;
    use xone_state::system_contracts::{dyn_array_slot, mapping_slot, validator_0x88 as v88};
    use xone_state::{Account, StateRead};

    struct Mem(HashMap<(Address, B256), U256>);
    impl StateRead for Mem {
        fn account(&self, _: Address) -> Option<Account> {
            None
        }
        fn storage(&self, address: Address, slot: B256) -> U256 {
            self.0.get(&(address, slot)).copied().unwrap_or(U256::ZERO)
        }
    }
    let put = |m: &mut HashMap<(Address, B256), U256>, slot: U256, v: U256| {
        m.insert((v88::ADDRESS, B256::from(slot.to_be_bytes::<32>())), v);
    };
    let (node, owner, voter) = (a(1), a(2), a(3));
    let mut m = HashMap::new();
    let key = node.into_word();
    let state = mapping_slot(key, v88::VALIDATORS_STATE);
    put(&mut m, state, U256::from_be_bytes(owner.into_word().0));
    let voters = mapping_slot(key, v88::VOTERS);
    put(&mut m, voters, U256::from(1u64));
    put(
        &mut m,
        dyn_array_slot(voters, 0, 1),
        U256::from_be_bytes(voter.into_word().0),
    );
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(voter.into_word().as_slice());
    b[32..].copy_from_slice(&state.wrapping_add(U256::from(2u64)).to_be_bytes::<32>());
    put(
        &mut m,
        U256::from_be_bytes(keccak256(b).0),
        U256::from(7u64),
    );
    let root = hash(1, 9);
    let h = Holders::read(root, &Mem(m), [node, a(9)]);
    let expect = Holders::new(
        root,
        [
            (node, owner, vec![(voter, U256::from(7u64))]),
            // No owner registered: the zero address, as geth pays it.
            (a(9), Address::ZERO, vec![]),
        ],
    );
    assert_eq!(h, expect);
}
