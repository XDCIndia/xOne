#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! The two rules of the leader's sibling that the xOneTS Gate D fault case A1 found (issue #128; TS
//! `packages/node/test/gate-d.test.ts` tests 13 and 14): no sibling when the head's round is at or above the
//! leader's round, and the transactions of the blocks taken off go back to the pool.

mod common;

use std::sync::Arc;

use alloy_primitives::{keccak256, B256};
use common::{parts, scratch, vectors};
use xone_builder::txpool::special::Authority;
use xone_chainspec::Network;
use xone_node::services::{
    core::{now, Core},
    pool::PoolShared,
    producer::{behind_head, reinject},
};
use xone_primitives::{BlockInfo, ExtraFieldsV2, Header, QuorumCert};

fn v2_header(round: u64) -> Header {
    let quorum_cert = QuorumCert {
        proposed_block_info: BlockInfo {
            hash: B256::ZERO,
            round: round.saturating_sub(1),
            number: 0,
        },
        signatures: vec![],
        gap_number: 0,
    };
    let extra = ExtraFieldsV2 { round, quorum_cert };
    Header {
        number: 1,
        extra_data: extra.encode_extra().into(),
        ..Header::default()
    }
}

/// TS test 13 (fault A1, round 2817, block 2840): the leader of round R was down, the network timed R out and
/// the next leader built a block of round R+1 on the QC block. A head of round R or above proves a QC or TC of
/// a round R or above, so a quorum has left R and votes only in its own round (`engine_v2/vote.go:414-421`): an R
/// sibling can never be certified, and geth keeps its head (`core/blockchain.go:2413-2424`).
#[test]
fn a_leader_whose_round_is_behind_the_heads_round_never_takes_the_head_off() {
    let head = v2_header(2818);
    assert!(behind_head(&head, 2817), "our round is behind the head's");
    assert!(behind_head(&head, 2818), "our round is the head's");
    assert!(
        !behind_head(&head, 2819),
        "the head is of an earlier round: the sibling is legitimate"
    );
    assert!(
        !behind_head(&Header::default(), 1),
        "a header with no V2 extra says nothing about a round"
    );
}

/// TS test 14: after the unwind the pool reads the QC block, then the transactions of every block taken off go
/// back, oldest block first (geth `core/txpool/legacypool/legacypool.go` `reset`, the `reinject` set). The
/// Apothem vectors carry the sign transactions of block 15 in blocks 16 and 17.
#[test]
fn the_transactions_of_the_blocks_a_sibling_takes_off_go_back_to_the_pool() {
    let net = Network::Apothem;
    let (spec, genesis) = (net.spec(), net.genesis().unwrap());
    let dir = scratch("sibling-reinject");
    let core = Arc::new(Core::open(spec, &genesis, &dir.join("xone"), 0).unwrap());
    for b in &vectors(17)[1..=17] {
        let (h, body) = parts(b);
        core.import(h, body, now()).unwrap();
    }
    let pool = PoolShared::new(core.clone()).unwrap();
    let dropped = core.unwind_to(15).unwrap();
    assert_eq!(
        dropped.iter().map(|s| s.header.number).collect::<Vec<_>>(),
        [16, 17]
    );
    // the senders are the candidates of the chain: the pool admits their sign transactions
    pool.set_authority(Authority::candidates(
        dropped
            .iter()
            .flat_map(|s| s.signs.iter().map(|t| t.sender)),
    ));
    let taken: Vec<B256> = dropped
        .iter()
        .flat_map(|s| s.body.transactions.iter().map(keccak256))
        .collect();
    assert!(!taken.is_empty(), "blocks 16 and 17 carry transactions");
    assert_eq!(pool.pending_count(), 0);
    assert_eq!(reinject(&pool, &dropped), taken.len());
    let (_, raws) = pool.pending_raws(0);
    for h in &taken {
        assert!(raws.contains_key(h), "{h} is back in the pool");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
