#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Deterministic construction (XB-23): 200 V1 and 200 V2 blocks of the Apothem replay vectors are rebuilt
//! from their own transactions, timestamp, round and QC, and must give the canonical header hash. A valid
//! producer need not choose the order of the transactions; given the order it must give the same bytes.
//! The state-dependent roots come from the recorded header (execution has no state here; the replay
//! harness proves them on the same blocks), and the signatures from the real masternodes, after the test
//! has checked that the digest the producer built is the one that masternode signed.

mod producer_common;

use alloy_primitives::U256;
use alloy_trie::root::ordered_trie_root_encoded;
use producer_common::{addrs, replay, Recorded};
use xone_builder::producer::{
    build::{assemble, Roots},
    gaslimit, seal_v1, seal_v2, timing,
};
use xone_chainspec::Network;
use xone_consensus::v1::{checkpoint::masternodes_of, turn::difficulty};
use xone_primitives::Header;

fn roots(h: &Header, txs: &[alloy_primitives::Bytes]) -> Roots {
    let tx_root = ordered_trie_root_encoded(txs);
    assert_eq!(
        tx_root, h.tx_root,
        "block {}: the transactions give the recorded root",
        h.number
    );
    Roots {
        state_root: h.state_root,
        tx_root,
        receipt_root: h.receipt_root,
        bloom: h.bloom,
        gas_used: h.gas_used,
        bal_hash: h.block_access_list_hash,
    }
}

#[test]
fn two_hundred_v1_blocks_are_rebuilt_to_the_canonical_hash() {
    let spec = Network::Apothem.spec();
    let epoch = spec.xdpos.epoch;
    // 800 is the parent; 801..=1000 holds the checkpoint 900 and the double validation after it
    let blocks = replay("apothem-0-1800.jsonl.gz", 800, 1000);
    assert_eq!(blocks.len(), 201);
    let cp = &blocks[100].header; // block 900
    assert_eq!(cp.number, 900);
    let mut m2_by_other = 0;
    for w in blocks.windows(2) {
        let (p, b) = (&w[0].header, &w[1].header);
        let creator = b.v1_signer().unwrap();
        let checkpoint = (b.number % epoch == 0).then(|| seal_v1::Checkpoint {
            masternodes: masternodes_of(&b.extra_data).unwrap(),
            penalties: b
                .penalties
                .clone()
                .to_vec()
                .chunks_exact(20)
                .map(alloy_primitives::Address::from_slice)
                .collect(),
            validators: b.validators.clone(),
        });
        let time = timing::v1_block_time(p.timestamp, spec.xdpos.period, b.timestamp);
        assert_eq!(
            time, b.timestamp,
            "block {} is on or after its slot",
            b.number
        );
        let h = seal_v1::prepare(&seal_v1::Prepare {
            parent: p,
            gas_limit: b.gas_limit,
            base_fee: None,
            difficulty: i64::try_from(b.difficulty).unwrap(),
            time,
            checkpoint,
            vanity: &b.extra_data[..32],
        });
        let h = assemble(h, &roots(b, &w[1].txs));
        let n = b.extra_data.len();
        let mut m1 = Recorded::new(creator, &b.extra_data[n - 65..]);
        let tip_randomize = spec.effective.tip_randomize.is_active(b.number);
        let sealed = seal_v1::seal(h, &mut m1, epoch, Some(cp), tip_randomize).unwrap();
        let mut h = sealed.header;
        if let Some(m2) = sealed.needs_m2 {
            m2_by_other += 1;
            let mut cosigner = Recorded::new(m2, &b.validator);
            h = seal_v1::cosign(h, creator, &mut cosigner, epoch, Some(cp), tip_randomize)
                .unwrap()
                .expect("this node is the assigned M2");
        }
        assert_eq!(h, *b, "block {}", b.number);
        assert_eq!(h.hash(), b.hash(), "block {}", b.number);
    }
    assert!(
        m2_by_other > 0,
        "the sample holds blocks whose M2 is another masternode"
    );
}

#[test]
fn two_hundred_v2_blocks_are_rebuilt_to_the_canonical_hash() {
    let spec = Network::Apothem.spec();
    let v2 = spec.xdpos.v2.as_ref().unwrap();
    // the first V2 epoch switch is the block after the V2 switch block
    let from = v2.switch_block;
    let blocks = replay("apothem-v2-56827800-56830294.jsonl.gz", from, from + 200);
    assert_eq!(blocks.len(), 201);
    let mut switches = 0;
    for w in blocks.windows(2) {
        let (p, b) = (&w[0].header, &w[1].header);
        let f = b.extra_fields_v2().unwrap();
        let mine_period = v2.config_at_round(f.round).unwrap().mine_period;
        let time = timing::v2_block_time(p.timestamp, mine_period, b.timestamp);
        assert_eq!(time, b.timestamp, "block {}", b.number);
        assert_eq!(
            gaslimit::base_fee(&spec, b.number).map(U256::from),
            b.base_fee
        );
        let switch =
            (!b.validators.is_empty()).then(|| (addrs(&b.validators), addrs(&b.penalties)));
        switches += usize::from(switch.is_some());
        let h = seal_v2::prepare(&seal_v2::Prepare {
            parent: p,
            round: f.round,
            qc: &f.quorum_cert,
            me: b.coinbase,
            gas_limit: b.gas_limit,
            base_fee: None,
            time,
            switch,
        })
        .unwrap();
        let h = assemble(h, &roots(b, &w[1].txs));
        let mut m = Recorded::new(b.coinbase, &b.validator);
        let h = seal_v2::seal(h, f.round, &mut m).unwrap();
        assert_eq!(m.asked.len(), 1);
        assert_eq!(h, *b, "block {}", b.number);
        assert_eq!(h.hash(), b.hash(), "block {}", b.number);
    }
    assert!(switches >= 1, "the sample holds an epoch switch");
    let _ = difficulty;
}

/// A lone masternode is its own M2: the one signature is both the seal and the `validator`, and the
/// verifier's double validation accepts it. No block of the replay sample has this case (the masternodes
/// there are several), so it is built here with a throwaway key.
#[test]
fn a_lone_masternode_is_its_own_m2() {
    use producer_common::{KeySealer, TestKey};
    use xone_builder::producer::seal_v1::Checkpoint;
    use xone_consensus::{
        unit::Sealer,
        v1::double_validation::{assigned_validator, check_double_validation},
    };
    let spec = Network::Apothem.spec();
    let epoch = spec.xdpos.epoch;
    let mut unit = KeySealer(TestKey::new(7));
    let me = unit.address();
    // the checkpoint 900: one masternode and the M2 token "0" (4 bytes, `BuildValidatorFromM2`)
    let mk = |parent: &Header, checkpoint| {
        seal_v1::prepare(&seal_v1::Prepare {
            parent,
            gas_limit: 420_000_000,
            base_fee: None,
            difficulty: 1,
            time: parent.timestamp + 2,
            checkpoint,
            vanity: &[],
        })
    };
    let genesis_side = Header {
        number: epoch - 1,
        timestamp: 1_000,
        ..Header::default()
    };
    let cp = mk(
        &genesis_side,
        Some(Checkpoint {
            masternodes: vec![me],
            penalties: vec![],
            validators: vec![0, 0, 0, b'0'].into(),
        }),
    );
    assert_eq!(cp.number, 900);
    let cp = seal_v1::seal(cp, &mut unit, epoch, None, true)
        .unwrap()
        .header;
    let next = mk(&cp, None);
    let sealed = seal_v1::seal(next, &mut unit, epoch, Some(&cp), true).unwrap();
    assert_eq!(sealed.needs_m2, None);
    let h = sealed.header;
    let n = h.extra_data.len();
    assert_eq!(
        h.validator[..],
        h.extra_data[n - 65..],
        "the seal and the validator are one signature"
    );
    assert_eq!(h.v1_signer().unwrap(), me);
    let m2 = check_double_validation(&h, me, || {
        assigned_validator(&h, me, Some(&cp), epoch, true)
    })
    .unwrap();
    assert_eq!(m2, me);
}
