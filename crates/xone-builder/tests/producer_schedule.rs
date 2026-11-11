#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! The schedule and the timing rules against real blocks: every canonical block was produced in a slot the
//! producer's rules allow, and the producer's timestamp for it is the recorded one. 50 consecutive blocks of
//! each run: Apothem V1, Apothem V2 (the replay window and the 2026 head), net2040 (head).

mod producer_common;

use alloy_primitives::{Address, B256};
use producer_common::{addrs, replay, root, Recorded};
use serde_json::Value;
use std::time::Duration;
use xone_builder::producer::{
    schedule::{self, Skip, V1Slot},
    timing,
};
use xone_chainspec::Network;
use xone_consensus::{
    unit::Sealer,
    v1::{checkpoint::masternodes_of, turn},
    v2::{masternodes, safety::Kind},
};
use xone_primitives::ExtraFieldsV2;

/// The V1 rule on 200 blocks: the creator of each block was in turn, or out of turn after its wait, and the
/// difficulty says which. A block is never earlier than the slot rule allows.
#[test]
fn every_v1_block_is_in_a_slot_the_schedule_allows() {
    let spec = Network::Apothem.spec();
    let epoch = spec.xdpos.epoch;
    let blocks = replay("apothem-0-1800.jsonl.gz", 899, 1100);
    let cp = &blocks[1].header; // 900
    assert_eq!(cp.number, 900);
    let list = masternodes_of(&cp.extra_data).unwrap();
    let (mut in_turn, mut out) = (0, 0);
    for w in blocks[1..].windows(2) {
        let (p, b) = (&w[0].header, &w[1].header);
        let (creator, parent_creator) = (b.v1_signer().unwrap(), p.v1_signer().unwrap());
        let slot = schedule::v1_slot(&list, p.number, Some(parent_creator), creator, epoch).unwrap();
        assert!(schedule::v1_may_seal(slot, p.timestamp, b.timestamp), "block {}: {slot:?}", b.number);
        let want = turn::difficulty(&list, p.number, Some(parent_creator), creator).unwrap();
        assert_eq!(i64::try_from(b.difficulty).unwrap(), want, "block {}", b.number);
        match slot {
            V1Slot::InTurn => in_turn += 1,
            V1Slot::OutOfTurn { .. } => out += 1,
            V1Slot::No => panic!("block {} by {creator} is in no slot", b.number),
        }
        assert_eq!(timing::v1_block_time(p.timestamp, spec.xdpos.period, b.timestamp), b.timestamp);
    }
    eprintln!("v1: {in_turn} in turn, {out} out of turn");
    assert_eq!(in_turn + out, 200);
}

/// Out of turn, by the rule of geth `YourTurn` (`xdpos.go:2010-2033`): ten seconds a hop, twenty once the hops
/// reach the next checkpoint; no slot for a node outside the list, or after a genesis parent that is not first.
#[test]
fn an_out_of_turn_masternode_waits_ten_seconds_a_hop() {
    let m: Vec<Address> = (1..=5).map(Address::repeat_byte).collect();
    let slot = |parent: u64, pre: usize, cur: usize| {
        schedule::v1_slot(&m, parent, Some(m[pre]), m[cur], 900).unwrap()
    };
    assert_eq!(slot(100, 1, 2), V1Slot::InTurn);
    assert_eq!(slot(100, 1, 3), V1Slot::OutOfTurn { wait: 10 });
    assert_eq!(slot(100, 1, 4), V1Slot::OutOfTurn { wait: 20 });
    assert_eq!(slot(100, 1, 0), V1Slot::OutOfTurn { wait: 30 });
    assert_eq!(slot(100, 1, 1), V1Slot::OutOfTurn { wait: 40 }, "the creator of the parent is last");
    // 3 hops reach the checkpoint at 900 from block 897: 20 s a hop
    assert_eq!(slot(897, 1, 0), V1Slot::OutOfTurn { wait: 60 });
    assert_eq!(slot(899, 1, 3), V1Slot::OutOfTurn { wait: 20 });
    let s = slot(100, 1, 3);
    assert!(!schedule::v1_may_seal(s, 1_000, 1_009) && schedule::v1_may_seal(s, 1_000, 1_010));
    assert_eq!(
        schedule::v1_slot(&m, 100, Some(m[1]), Address::repeat_byte(9), 900).unwrap(),
        V1Slot::No
    );
    // the first block: only the first masternode is in turn
    assert_eq!(schedule::v1_slot(&m, 0, None, m[0], 900).unwrap(), V1Slot::InTurn);
    assert_eq!(schedule::v1_slot(&m, 0, None, m[1], 900).unwrap(), V1Slot::No);
}

/// The V2 rule on 200 blocks: the coinbase of each block leads its round in the list of its epoch, and the
/// mined-round guard of the unit lets exactly that round through.
#[test]
fn every_v2_block_is_led_by_the_leader_of_its_round() {
    let spec = Network::Apothem.spec();
    let v2 = spec.xdpos.v2.as_ref().unwrap();
    let blocks = replay("apothem-v2-56827800-56830294.jsonl.gz", v2.switch_block, v2.switch_block + 200);
    let mut list = masternodes_of(&blocks[0].header.extra_data).unwrap();
    let mut led = 0;
    for b in &blocks[1..] {
        let h = &b.header;
        if !h.validators.is_empty() {
            list = addrs(&h.validators);
        }
        let f = h.extra_fields_v2().unwrap();
        let mut me = Recorded::new(h.coinbase, &h.validator);
        assert_eq!(schedule::v2_turn(&me, f.round, spec.xdpos.epoch, &list), Ok(()), "block {}", h.number);
        led += 1;
        // a node that already signed this round, or a later one, does not propose it
        struct Mined(Address, u64);
        impl Sealer for Mined {
            fn address(&self) -> Address { self.0 }
            fn signed_up_to(&self, _: Kind) -> u64 { self.1 }
            fn sign(&mut self, _: xone_consensus::v2::safety::Intent) -> Result<xone_primitives::Signature, xone_consensus::unit::UnitError> { unreachable!() }
            fn cosign_v1(&mut self, _: u64, _: B256) -> Result<xone_primitives::Signature, xone_consensus::unit::UnitError> { unreachable!() }
            fn sign_tx(&mut self, _: &B256) -> Result<xone_primitives::Signature, xone_consensus::unit::UnitError> { unreachable!() }
        }
        assert_eq!(
            schedule::v2_turn(&Mined(h.coinbase, f.round), f.round, spec.xdpos.epoch, &list),
            Err(Skip::AlreadyMined { round: f.round, highest: f.round })
        );
        // another masternode is not the leader
        let other = list.iter().copied().find(|a| *a != h.coinbase).unwrap();
        assert_eq!(
            schedule::v2_turn(&Mined(other, 0), f.round, spec.xdpos.epoch, &list),
            Err(Skip::NotLeader { leader: h.coinbase })
        );
        me.sig.clear();
    }
    assert_eq!(led, 200);
    let _ = masternodes::leader;
}

fn heads(file: &str, from: u64) -> Vec<(u64, u64, Vec<u8>)> {
    let t = std::fs::read_to_string(root().join("conformance/vectors/headers").join(file)).unwrap();
    let mut rows: Vec<(u64, u64, Vec<u8>)> = t
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|v| {
            let hex = |k: &str| u64::from_str_radix(v[k].as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
            let extra = alloy_primitives::hex::decode(v["extraData"].as_str().unwrap()).unwrap();
            (hex("number"), hex("timestamp"), extra)
        })
        .filter(|r| (from..from + 50).contains(&r.0))
        .collect();
    rows.sort_by_key(|r| r.0);
    assert_eq!(rows.len(), 50, "{file} holds 50 consecutive blocks from {from}");
    rows
}

/// 50 consecutive V2 blocks: the timestamp is the engine's `max(parent + minePeriod(round), now)` for the
/// recorded time, a block never comes before its period, and the worker's wait to the next slot is the
/// period less what has passed.
fn v2_run(rows: &[(u64, u64, Vec<u8>)], mine_period_of: impl Fn(u64) -> u64) -> usize {
    let mut on_slot = 0;
    for w in rows.windows(2) {
        let ((_, pt, _), (n, bt, extra)) = (&w[0], &w[1]);
        let round = ExtraFieldsV2::decode_extra(extra).unwrap().round;
        let mp = mine_period_of(round);
        assert_eq!(timing::v2_block_time(*pt, mp, *bt), *bt, "block {n}");
        assert!(timing::v2_period_elapsed(*pt, mp, *bt), "block {n}");
        // the timer after the parent: one period, less the milliseconds already past, never more
        let waited = timing::reset_time(*pt, mp, pt * 1000 + 300);
        assert_eq!(waited, Duration::from_millis(mp * 1000 - 300));
        on_slot += usize::from(*bt == pt + mp);
    }
    on_slot
}

#[test]
fn fifty_consecutive_blocks_per_network_follow_the_engines_schedule() {
    let apothem = Network::Apothem.spec();
    let v2 = apothem.xdpos.v2.as_ref().unwrap();
    let mp = |r| v2.config_at_round(r).unwrap().mine_period;
    // Apothem V1: the replay vectors
    let v1 = replay("apothem-0-1800.jsonl.gz", 1000, 1050);
    for w in v1.windows(2) {
        let (p, b) = (&w[0].header, &w[1].header);
        assert_eq!(timing::v1_block_time(p.timestamp, apothem.xdpos.period, b.timestamp), b.timestamp);
    }
    // Apothem V2: the replay window after the switch, and the head of the archive
    let win = replay("apothem-v2-56827800-56830294.jsonl.gz", v2.switch_block + 1, v2.switch_block + 51);
    let rows: Vec<_> = win.iter().map(|b| (b.header.number, b.header.timestamp, b.header.extra_data.to_vec())).collect();
    let on = v2_run(&rows, mp);
    let head = v2_run(&heads("apothem.jsonl", 85_227_973), mp);
    // net2040 (head vectors): its V2 config row has minePeriod 2 (the blocks never come sooner)
    let net = v2_run(&heads("net2040.jsonl", 730_423), |_| 2);
    eprintln!("blocks on their slot of 49: replay {on}, apothem head {}, net2040 {net}", on.min(head));
    assert!(on + head + net > 0);
    // geth `getResetTimeXDC` (`miner/xdpos_worker.go:274-290`)
    let ms = Duration::from_millis;
    assert_eq!(timing::reset_time(100, 2, 100_500), ms(1500));
    assert_eq!(timing::reset_time(100, 2, 99_000), ms(2000), "a head in the future waits one period");
    assert_eq!(timing::reset_time(100, 2, 102_000), ms(2000), "a slot already past waits one period");
    assert_eq!(timing::reset_time(100, 2, 103_000), ms(2000));
    // geth V1 `Seal`: the release delay, and the wiggle of a difficulty-1 block
    assert_eq!(timing::v1_release_delay(100, 99_000, 2, 5, 0), ms(1000));
    assert_eq!(timing::v1_release_delay(100, 101_000, 2, 5, 0), ms(0));
    assert_eq!(timing::v1_release_delay(100, 99_000, 1, 5, 7_777), ms(1000 + 7_777 % 1500));
    assert_eq!(timing::v1_release_delay(100, 99_000, 1, 1, 1_000), ms(1000 + 1_000 % 500), "a lone masternode");
    assert!(timing::v1_parent_second_passed(100, 101) && !timing::v1_parent_second_passed(100, 100));
}
