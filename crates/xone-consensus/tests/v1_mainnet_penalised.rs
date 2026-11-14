#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! Mainnet (chain 50) V1 checkpoints that carry `Penalties`. A penalised masternode is still a signer
//! of the snapshot but is not in the checkpoint's list (geth `checkSignersOnCheckpoint`,
//! `consensus/XDPoS/xdpos.go:1014-1062`), so a checkpoint is checked against the signers minus the
//! penalties of its own header and of the four checkpoints before it. The first one is 469,800: the
//! follower stopped there because it compared the list with the raw snapshot.
//! Vectors: `conformance/vectors/consensus/v1/mainnet_penalised.jsonl`, written by
//! `gen_mainnet_penalised.py` from the public RPC; the header is re-encoded and its hash checked.

use std::collections::BTreeMap;
use std::path::PathBuf;

use alloy_primitives::{address, Address, Bytes, B256};
use serde::Deserialize;
use xone_consensus::penalty::same_set;
use xone_consensus::v1::checkpoint::{addresses, checkpoint_signers, masternodes_of};
use xone_primitives::Header;

const EPOCH: u64 = 900;

#[derive(Deserialize)]
struct Earlier {
    number: u64,
    penalties: Bytes,
}

#[derive(Deserialize)]
struct Row {
    number: u64,
    hash: B256,
    header: Bytes,
    earlier: Vec<Earlier>,
}

fn rows() -> Vec<Row> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/consensus/v1/mainnet_penalised.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// What a row gives: the header, its listed masternodes, the penalties of the four checkpoints
/// before it, and a snapshot signer set built from them (the list, plus everything a header of the
/// window penalises: a penalised masternode is a candidate of the gap block all the same).
fn parts(
    r: &Row,
) -> (
    Header,
    Vec<Address>,
    BTreeMap<u64, Vec<Address>>,
    Vec<Address>,
) {
    let h: Header = alloy_rlp::decode_exact(&r.header).unwrap();
    assert_eq!((h.number, h.hash()), (r.number, r.hash));
    let listed = masternodes_of(&h.extra_data).unwrap();
    let earlier: BTreeMap<_, _> = r
        .earlier
        .iter()
        .map(|e| (e.number, addresses(&e.penalties)))
        .collect();
    let mut signers = listed.clone();
    signers.extend(addresses(&h.penalties));
    signers.extend(earlier.values().flatten());
    signers.sort();
    signers.dedup();
    (h, listed, earlier, signers)
}

const PENALISED_469800: Address = address!("a0caec455e7585d845bbf72420397774e52f3b74");

#[test]
fn block_469800_drops_its_penalised_signer() {
    let all = rows();
    let r = all.iter().find(|r| r.number == 469_800).unwrap();
    let (h, listed, earlier, signers) = parts(r);
    // the header: 17 listed, one penalty, which is a signer of the snapshot
    assert_eq!(listed.len(), 17);
    assert_eq!(addresses(&h.penalties), [PENALISED_469800]);
    assert_eq!(signers.len(), 18);
    // the raw snapshot is not the list: the rule the follower stopped on
    assert!(!same_set(&listed, &signers));
    // the rule of geth: snapshot minus penalties
    let want = checkpoint_signers(
        &signers,
        h.number,
        EPOCH,
        &addresses(&h.penalties),
        &earlier,
    )
    .unwrap();
    assert!(same_set(&listed, &want));
}

#[test]
fn a_penalty_holds_for_this_and_the_next_four_epochs() {
    // 469,800 penalises the address; 470,700 .. 473,400 (four epochs) still drop it though their own
    // penalties are empty; at 474,300 it is a masternode again (`LimitPenaltyEpoch` = 4).
    for r in rows()
        .iter()
        .filter(|r| (470_700..=474_300).contains(&r.number))
    {
        let (h, listed, earlier, mut signers) = parts(r);
        let back = r.number - EPOCH * 4 <= 469_800;
        assert_eq!(
            !listed.contains(&PENALISED_469800),
            back,
            "list of {}",
            r.number
        );
        let want = checkpoint_signers(
            &signers,
            h.number,
            EPOCH,
            &addresses(&h.penalties),
            &earlier,
        )
        .unwrap();
        assert!(same_set(&listed, &want), "{}", r.number);
        // without the carried penalties the list is wrong: the earlier epochs matter
        if !signers.contains(&PENALISED_469800) {
            signers.push(PENALISED_469800);
        }
        let none = earlier.keys().map(|k| (*k, vec![])).collect();
        let late = checkpoint_signers(&signers, h.number, EPOCH, &[], &none).unwrap();
        assert_eq!(same_set(&listed, &late), !back, "{}", r.number);
    }
}

#[test]
fn every_row_matches_and_a_listed_penalty_does_not() {
    for r in rows() {
        let (h, listed, earlier, signers) = parts(&r);
        let own = addresses(&h.penalties);
        let want = checkpoint_signers(&signers, h.number, EPOCH, &own, &earlier).unwrap();
        assert!(same_set(&listed, &want), "{}", r.number);
        // a header that lists a penalised signer is refused
        if let Some(p) = own.first() {
            let mut bad = listed.clone();
            bad.push(*p);
            assert!(!same_set(&bad, &want), "{}", r.number);
        }
    }
}
