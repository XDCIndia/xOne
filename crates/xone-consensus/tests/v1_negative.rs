#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The V1 rules reject what they must, with the typed error that names the rule: the pairs of
//! `conformance/vectors/consensus/v1/negative.jsonl` (written by
//! `cargo run -p xone-negative-gen --bin xone-negative-gen-v1`), the typed error of every missing
//! input, and the genesis `XDPoS.period` of the timestamp check (xOneGo #682).

#[path = "../../../conformance/negative-gen/src/v1case.rs"]
#[allow(dead_code)]
mod v1case;
#[path = "../../../conformance/negative-gen/src/vector.rs"]
#[allow(dead_code)]
mod vector;

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use v1case::Case;
use xone_consensus::v1::{
    check_timestamp,
    checkpoint::{checkpoint_masternodes, checkpoint_penalties, select_candidates},
    double_validation::assigned_validator,
    penalty::{penalties_v1, PenaltyEra},
    snapshot::Snapshot,
    switch::{check_switch_block, switch_masternodes},
    turn::difficulty,
};
use xone_consensus::verify::{Context, HeaderError, HeaderVerifier, V1Context};
use xone_primitives::Header;

const RULES: [&str; 12] = [
    "v1_wrong_m2",
    "v1_validator_missing",
    "v1_validator_unrecoverable",
    "v1_creator_not_authorised",
    "v1_creator_signed_the_parent",
    "v1_difficulty_out_of_turn_claims_in_turn",
    "v1_difficulty_in_turn_claims_out_of_turn",
    "v1_checkpoint_list_dropped",
    "v1_checkpoint_list_extra",
    "v1_checkpoint_list_reordered",
    "v1_checkpoint_penalties_wrong",
    "v1_switch_block_not_epoch_aligned",
];

#[test]
fn every_negative_case_is_rejected_with_its_error_and_its_twin_is_accepted() {
    let text = std::fs::read_to_string(vector::vectors_dir().join("v1/negative.jsonl")).unwrap();
    let cases: Vec<Case> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut per_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &cases {
        assert_eq!(
            c.accept.run(),
            Ok(()),
            "{}: the unmutated input must be accepted",
            c.id
        );
        match c.reject.run() {
            Err(e) => assert_eq!(e.code(), c.expect, "{}: {}", c.id, c.recipe),
            Ok(()) => panic!("{}: accepted ({})", c.id, c.recipe),
        }
        *per_rule.entry(&c.rule).or_default() += 1;
    }
    for r in RULES {
        assert!(per_rule.contains_key(r), "no case for rule {r}");
    }
    println!(
        "{} negative cases, {} rules: {per_rule:?}",
        cases.len(),
        per_rule.len()
    );
}

fn addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

#[test]
fn every_missing_input_is_a_typed_error_not_an_empty_list() {
    let m = [addr(1), addr(2), addr(3)];
    // Header: the checkpoint of the M2 assignment is absent.
    let h = Header {
        number: 1000,
        ..Default::default()
    };
    assert_eq!(
        assigned_validator(&h, addr(1), None, 900, true),
        Err(HeaderError::MissingCheckpointHeader(900))
    );
    // Header: the creator of the parent, and the masternode list, for the turn.
    assert_eq!(
        difficulty(&m, 10, None, addr(1)),
        Err(HeaderError::MissingParentCreator)
    );
    assert_eq!(
        difficulty(&[], 10, Some(addr(2)), addr(1)),
        Err(HeaderError::MissingMasternodes)
    );
    // Snapshot input.
    let snap = Snapshot::new(0, B256::ZERO, []);
    assert_eq!(snap.inturn(5, addr(1)), Err(HeaderError::MissingSigners));
    // Signer state: no candidate in the 0x88 state, no penalty header, no signing data.
    assert_eq!(
        select_candidates(&[], 18),
        Err(HeaderError::MissingCandidates)
    );
    assert_eq!(
        checkpoint_penalties(2700, 900, &[], &BTreeMap::new()),
        Err(HeaderError::MissingPenaltyHeader(1800))
    );
    assert_eq!(
        penalties_v1(PenaltyEra::Creators, &m, &BTreeMap::new()),
        Err(HeaderError::MissingSignData)
    );
    assert_eq!(
        penalties_v1(PenaltyEra::Creators, &[], &BTreeMap::from([(addr(1), 3)])),
        Err(HeaderError::MissingMasternodes)
    );
    // A checkpoint with every candidate penalised has no list.
    let cands = [(addr(1), U256::from(5)), (addr(2), U256::from(4))];
    assert_eq!(
        checkpoint_masternodes(&cands, 18, &[addr(1), addr(2)]),
        Err(HeaderError::EmptyCheckpointList)
    );
    // The switch block: a short extra data, a zero epoch.
    let short = Header {
        extra_data: vec![0; 40].into(),
        ..Default::default()
    };
    assert_eq!(
        switch_masternodes(&short),
        Err(HeaderError::MissingSignature)
    );
    let mut x = xone_chainspec::Network::Apothem.spec().xdpos;
    x.epoch = 0;
    assert_eq!(check_switch_block(&x), Err(HeaderError::ZeroEpoch));
    // Snapshot headers that are not a chain after the snapshot.
    let skip = Header {
        number: 5,
        ..Default::default()
    };
    assert_eq!(
        snap.apply(&[skip], 900),
        Err(HeaderError::InvalidVotingChain)
    );
}

/// A V1 block of the corpus with a gap to its parent of at least 3 s, so a period of 2 s and a
/// period of its own gap differ.
fn slow_block() -> (vector::Vector, u64) {
    let all = vector::load_file(&vector::vectors_dir().join("valid/apothem.jsonl"));
    all.into_iter()
        .filter(|v| v.context.consensus == "v1" && v.number % 900 != 0)
        .find_map(|v| {
            let (h, p): (Header, Header) = (
                alloy_rlp::decode_exact(&v.header).unwrap(),
                alloy_rlp::decode_exact(&v.parent).unwrap(),
            );
            let gap = h.timestamp - p.timestamp;
            (gap >= 3).then_some((v, gap))
        })
        .expect("a V1 block with a gap of 3 s or more")
}

fn verify_with_period(v: &vector::Vector, period: u64) -> Result<(), HeaderError> {
    let mut spec = vector::spec_of(v);
    spec.xdpos.period = period;
    let c = &v.context;
    HeaderVerifier::new(&spec).verify(
        &alloy_rlp::decode_exact(&v.header).unwrap(),
        &alloy_rlp::decode_exact(&v.parent).unwrap(),
        c.now,
        &Context::V1(Box::new(V1Context {
            masternodes: c.masternodes.clone(),
            parent_masternodes: c.parent_masternodes.clone(),
            signers: c.signers.clone(),
            checkpoint: c
                .checkpoint
                .as_ref()
                .map(|b| alloy_rlp::decode_exact(b).unwrap()),
            checkpoint_signers: c.checkpoint_signers.clone(),
        })),
    )
}

/// xOneGo #682: the minimum gap to the parent is the genesis `XDPoS.period`, whatever it is, and
/// not a constant: a block of a fast chain (period 1) is valid 1 s after its parent.
#[test]
fn the_v1_timestamp_check_uses_the_genesis_period() {
    // A fast-period spec: 1 s apart is valid, 0 s is not; the same gap is too early at 2 s.
    assert_eq!(check_timestamp(100, 101, 1), Ok(()));
    assert!(check_timestamp(100, 100, 1).is_err());
    assert!(matches!(
        check_timestamp(100, 101, 2),
        Err(HeaderError::InvalidTimestamp { period: 2, .. })
    ));
    assert_eq!(check_timestamp(100, 102, 2), Ok(()));

    // The same through the header verifier, on a real block `gap` seconds after its parent.
    let (v, gap) = slow_block();
    for period in [0, 1, gap] {
        assert_eq!(verify_with_period(&v, period), Ok(()), "period {period}");
    }
    assert_eq!(
        verify_with_period(&v, gap + 1),
        Err(HeaderError::InvalidTimestamp {
            time: v.context.now,
            parent: v.context.now - gap,
            period: gap + 1
        })
    );
    let spec = vector::spec_of(&v);
    assert_eq!(spec.xdpos.period, 2, "Apothem's own period");
}

/// The rule is "fewer than `MinimunMinerBlockPerEpoch` (1) counted blocks": a masternode with one
/// block is not penalised, one with none is (geth `hooks.go:231-240`).
#[test]
fn a_masternode_with_one_counted_block_is_not_penalised() {
    let m = [addr(1), addr(2), addr(3)];
    let counts = BTreeMap::from([(addr(1), 1), (addr(3), 5)]);
    for era in [PenaltyEra::BlockSigners, PenaltyEra::Creators] {
        assert_eq!(penalties_v1(era, &m, &counts), Ok(vec![addr(2)]));
    }
    assert_eq!(
        penalties_v1(PenaltyEra::ComebackOnly, &m, &counts),
        Ok(vec![])
    );
}
