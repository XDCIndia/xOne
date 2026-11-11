#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    reason = "test code: fixtures are trusted inputs and a panic is the failure signal"
)]
//! The bootstrap anchor, the retention table and the pivot plan (XA-09, issue #14).
//!
//! Real data: `tests/bootstrap/` holds headers and answers of the Apothem archive (`xdc02:21105`,
//! read-only, 2026-11-11, geth-XDC at `conformance/PIN`); nothing in it is typed by hand. Synthetic
//! data: manifests signed with keys made here, for the cases a real chain cannot produce.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256, U64};
use k256::ecdsa::SigningKey;
use serde_json::Value;
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_consensus::chain::{
    BlockReader, Error as ChainError, HeaderView, PenaltyHistory, SignTx, SignTxCounter,
};
use xone_consensus::penalty;
use xone_consensus::reward;
use xone_consensus::v2::{
    cert::qc_digest,
    commit::{commit_blocks, Headers, Link},
    gap_snapshot, Error as V2Error,
};
use xone_primitives::{BlockInfo, BlockNonce, ExtraFieldsV2, Header, QuorumCert};
use xone_state::{Account, StateRead};
use xone_sync::bootstrap::{
    pivot_plan, plan_at, verify_anchor, Anchor, AnchorError, Class, Context, Error, Missing,
    Provenance, Retention, Rule, FORMAT_VERSION,
};

const E: u64 = 900;
const G: u64 = 450;

// ------------------------------------------------------------------ real fixtures

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../../tests/bootstrap/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

macro_rules! get {
    ($v:expr, $k:expr) => {
        serde_json::from_value($v[$k].clone()).unwrap_or_else(|e| panic!("{}: {e}", $k))
    };
}

fn qty(v: &Value, k: &str) -> u64 {
    let x: U64 = get!(v, k);
    x.to()
}

fn opt<T>(v: &Value, k: &str, f: impl Fn(&Value, &str) -> T) -> Option<T> {
    v.get(k).map(|_| f(v, k))
}

/// The RPC block object to a header (the field map of geth `gen_header_json.go`).
fn header_from_json(v: &Value) -> Header {
    Header {
        parent_hash: get!(v, "parentHash"),
        uncle_hash: get!(v, "sha3Uncles"),
        coinbase: get!(v, "miner"),
        state_root: get!(v, "stateRoot"),
        tx_root: get!(v, "transactionsRoot"),
        receipt_root: get!(v, "receiptsRoot"),
        bloom: get!(v, "logsBloom"),
        difficulty: get!(v, "difficulty"),
        number: qty(v, "number"),
        gas_limit: qty(v, "gasLimit"),
        gas_used: qty(v, "gasUsed"),
        timestamp: qty(v, "timestamp"),
        extra_data: get!(v, "extraData"),
        mix_digest: get!(v, "mixHash"),
        nonce: BlockNonce(get!(v, "nonce")),
        validators: get!(v, "validators"),
        validator: get!(v, "validator"),
        penalties: get!(v, "penalties"),
        base_fee: opt(v, "baseFeePerGas", |v, k| get!(v, k)),
        withdrawals_hash: opt(v, "withdrawalsRoot", |v, k| get!(v, k)),
        blob_gas_used: opt(v, "blobGasUsed", qty),
        excess_blob_gas: opt(v, "excessBlobGas", qty),
        parent_beacon_root: opt(v, "parentBeaconBlockRoot", |v, k| get!(v, k)),
        requests_hash: opt(v, "requestsHash", |v, k| get!(v, k)),
        block_access_list_hash: opt(v, "blockAccessListHash", |v, k| get!(v, k)),
        slot_number: opt(v, "slotNumber", qty),
    }
}

/// The headers of a fixture, each checked against the hash the archive reported.
fn headers_of(name: &str) -> Vec<Header> {
    fixture(name)
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            let h = header_from_json(&v);
            let want: B256 = get!(v, "hash");
            assert_eq!(h.hash(), want, "block {}", h.number);
            h
        })
        .collect()
}

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn ctx<'a>(pins: &'a [(u64, B256)]) -> Context<'a> {
    Context {
        now: NOW,
        pins,
        max_age_cap: 7 * DAY,
    }
}

fn anchor_at(
    spec: &ChainSpec,
    headers: &[Header],
    at: usize,
    tail: usize,
    validators: Vec<Address>,
) -> Anchor {
    let cp = &headers[at];
    Anchor {
        version: FORMAT_VERSION,
        chain_id: spec.chain_id,
        genesis_hash: spec.genesis_hash,
        number: cp.number,
        hash: cp.hash(),
        state_root: cp.state_root,
        created_at: NOW - 3600,
        max_age: DAY,
        provenance: Provenance {
            switch_number: headers[0].number,
            switch_hash: headers[0].hash(),
            validators,
        },
        headers: headers[..=at + tail].to_vec(),
    }
}

fn pin(a: &Anchor) -> [(u64, B256); 1] {
    [(a.number, a.hash)]
}

/// The Apothem epoch switch 84,954,819 and its next 15 blocks, with the checkpoint 12 blocks in.
fn real_v2() -> (ChainSpec, Anchor) {
    let spec = Network::Apothem.spec();
    let headers = headers_of("apothem-v2-84954819.jsonl");
    let validators = xone_consensus::v1::checkpoint::addresses(&headers[0].validators);
    let a = anchor_at(&spec, &headers, 12, 3, validators);
    (spec, a)
}

#[test]
fn real_apothem_anchor_verifies() {
    let (spec, a) = real_v2();
    assert_eq!(a.number, 84_954_831);
    let pins = pin(&a);
    assert_eq!(verify_anchor(&spec, &a, &ctx(&pins)), Ok(()));
    // The set the header gives is the set the archive's own API reports for the block.
    let v: Value = serde_json::from_str(&fixture("apothem-masternodes-84954831.json")).unwrap();
    let want: Vec<Address> = get!(v, "masternodes");
    assert_eq!(want.len(), 95);
    assert_eq!(a.provenance.validators, want);
    // The checkpoint itself may be the epoch switch: the shortest proof.
    let headers = headers_of("apothem-v2-84954819.jsonl");
    let v = a.provenance.validators.clone();
    let b = anchor_at(&spec, &headers, 0, 3, v);
    let pins = pin(&b);
    assert_eq!(verify_anchor(&spec, &b, &ctx(&pins)), Ok(()));
}

#[test]
fn real_anchor_survives_the_wire_format() {
    let (spec, a) = real_v2();
    let back = Anchor::decode(&a.encode()).unwrap();
    assert_eq!(back, a);
    let pins = pin(&back);
    assert_eq!(verify_anchor(&spec, &back, &ctx(&pins)), Ok(()));
}

#[test]
fn real_anchor_tampered_is_refused_with_the_typed_error() {
    let (spec, a) = real_v2();
    let pins = pin(&a);
    let run = |a: &Anchor| verify_anchor(&spec, a, &ctx(&pins));

    // the last header holds the QC of the block two above the checkpoint; no header follows it
    let edit_last_qc = |f: &dyn Fn(&mut QuorumCert)| {
        let mut b = a.clone();
        let last = b.headers.last_mut().unwrap();
        let mut fields = last.extra_fields_v2().unwrap();
        f(&mut fields.quorum_cert);
        last.extra_data = fields.encode_extra().into();
        b
    };
    // one signature byte flipped: it recovers some other key, who is not a masternode
    let forged = edit_last_qc(&|qc| {
        let mut s = qc.signatures[3].to_vec();
        s[10] ^= 1;
        qc.signatures[3] = s.into();
    });
    assert!(matches!(
        run(&forged),
        Err(AnchorError::ForgedCertificate(
            V2Error::SignerNotMasternode(_) | V2Error::SignatureUnrecoverable
        ))
    ));
    // the QC cut below two thirds of 95 masternodes (0.667 * 95 = 63.4, so 64)
    let short = edit_last_qc(&|qc| qc.signatures.truncate(40));
    assert_eq!(
        run(&short),
        Err(AnchorError::UnderThreshold {
            unique: 40,
            required: 64,
            masternodes: 95
        })
    );
    // one signature repeated to fill the QC counts once
    let dup = edit_last_qc(&|qc| {
        let first = qc.signatures[0].clone();
        qc.signatures = vec![first; 64];
    });
    assert_eq!(
        run(&dup),
        Err(AnchorError::UnderThreshold {
            unique: 1,
            required: 64,
            masternodes: 95
        })
    );
    // a root that is not the checkpoint's
    let mut root = a.clone();
    root.state_root = B256::repeat_byte(9);
    assert!(matches!(run(&root), Err(AnchorError::RootMismatch { .. })));
    // the proof ends before the third block above the checkpoint
    let mut short = a.clone();
    short.headers.pop();
    assert!(matches!(run(&short), Err(AnchorError::TooShort { .. })));
    // the same headers, validators swapped for others
    let mut swapped = a.clone();
    swapped.provenance.validators.reverse();
    assert_eq!(
        run(&swapped),
        Err(AnchorError::ProvenanceValidators(84_954_819))
    );
    // a manifest that names a checkpoint the headers do not hold
    let mut hash = a.clone();
    hash.hash = B256::repeat_byte(7);
    let claimed = [(hash.number, hash.hash)];
    assert!(matches!(
        verify_anchor(&spec, &hash, &ctx(&claimed)),
        Err(AnchorError::CheckpointHash { .. })
    ));
    assert_eq!(run(&hash), Err(AnchorError::NotPinned));
}

#[test]
fn real_anchor_in_the_first_v2_epoch_starts_above_the_v2_switch_block() {
    // Apothem V2 switch block 56,828,700. Block +1 is the first V2 block: it carries the list of
    // the first V2 epoch as `validators` (13 masternodes) and is the epoch switch the QCs above it
    // are checked against. The switch block itself is a V1 header.
    let spec = Network::Apothem.spec();
    let headers = headers_of("apothem-v2switch-56828700.jsonl");
    assert_eq!(headers[0].number, 56_828_700);
    let list = xone_consensus::v1::checkpoint::addresses(&headers[1].validators);
    assert_eq!(list.len(), 13);
    for at in [0, 11] {
        let a = anchor_at(&spec, &headers[1..], at, 3, list.clone());
        let pins = pin(&a);
        assert_eq!(
            verify_anchor(&spec, &a, &ctx(&pins)),
            Ok(()),
            "checkpoint {}",
            a.number
        );
    }
    // starting on the V1 switch block: it has no V2 list, and the block above it is a second switch
    let v1 = anchor_at(&spec, &headers, 12, 3, list.clone());
    let pins = pin(&v1);
    assert_eq!(
        verify_anchor(&spec, &v1, &ctx(&pins)),
        Err(AnchorError::ProvenanceValidators(56_828_700))
    );
}

#[test]
fn real_v1_anchor_verifies_by_seals() {
    let spec = Network::Apothem.spec();
    let headers = headers_of("apothem-v1-50000400.jsonl");
    let list = xone_consensus::v1::checkpoint::masternodes_of(&headers[0].extra_data).unwrap();
    assert_eq!(list.len(), 10);
    let a = anchor_at(&spec, &headers, 3, headers.len() - 4, list.clone());
    assert_eq!(a.number, 50_000_403);
    let pins = pin(&a);
    assert_eq!(verify_anchor(&spec, &a, &ctx(&pins)), Ok(()));
    let run = |a: &Anchor| verify_anchor(&spec, a, &ctx(&pins));

    // too shallow: 7 of 10 sealers are needed, ceil(2 * 10 / 3)
    let mut shallow = a.clone();
    shallow.headers.truncate(4 + 3);
    assert!(matches!(
        run(&shallow),
        Err(AnchorError::V1Depth { required: 7, .. })
    ));
    // a header sealed by a key that is not in the checkpoint list
    let mut other = a.clone();
    let mut list2 = list.clone();
    list2.pop();
    other.provenance.validators = list2;
    assert_eq!(
        run(&other),
        Err(AnchorError::ProvenanceValidators(50_000_400))
    );
    // the checkpoint block itself is not an anchor: its seal is of the previous list
    let b = anchor_at(&spec, &headers, 0, headers.len() - 1, list);
    let pins = pin(&b);
    assert_eq!(
        verify_anchor(&spec, &b, &ctx(&pins)),
        Err(AnchorError::V1Position(50_000_400))
    );
}

// ------------------------------------------------------------------ synthetic manifests

fn address_of(k: &SigningKey) -> Address {
    let p = k.verifying_key().to_encoded_point(false);
    Address::from_slice(&keccak256(&p.as_bytes()[1..])[12..])
}

fn sign(k: &SigningKey, digest: &B256) -> Bytes {
    let (sig, id) = k.sign_prehash_recoverable(digest.as_slice()).unwrap();
    let mut v = sig.to_bytes().to_vec();
    v.push(id.to_byte());
    v.into()
}

fn key(i: u8) -> SigningKey {
    SigningKey::from_slice(&[i + 1; 32]).unwrap()
}

const S: u64 = 85_000_100;
const R0: u64 = 28_800_000; // 32,000 * 900: the first round of an epoch

/// Which signatures the QC in header `i` carries; `i` is the index in the proof (1 is above the
/// epoch switch).
type Signer<'a> = &'a dyn Fn(usize, &B256) -> Vec<Bytes>;

/// An Apothem V2 proof of five headers, the epoch switch first, the checkpoint at index 1.
/// `rounds[i]` is the round of header `i`.
fn synthetic(masters: &[SigningKey], rounds: &[u64], gap: u64, signer: Signer<'_>) -> Anchor {
    let spec = Network::Apothem.spec();
    let set: Vec<Address> = masters.iter().map(address_of).collect();
    let mut headers: Vec<Header> = Vec::new();
    for (i, round) in rounds.iter().enumerate() {
        let mut h = Header {
            number: S + u64::try_from(i).unwrap(),
            state_root: keccak256(i.to_le_bytes()),
            difficulty: U256::from(1),
            ..Header::default()
        };
        let mut qc = QuorumCert {
            proposed_block_info: BlockInfo {
                hash: B256::repeat_byte(1),
                round: round - 1,
                number: S - 1,
            },
            signatures: vec![],
            gap_number: gap,
        };
        if let Some(prev) = headers.last() {
            h.parent_hash = prev.hash();
            qc.proposed_block_info = BlockInfo {
                hash: prev.hash(),
                round: rounds[i - 1],
                number: prev.number,
            };
            qc.signatures = signer(i, &qc_digest(&qc));
        } else {
            h.validators = set.iter().flat_map(|a| a.0).collect::<Vec<u8>>().into();
        }
        h.extra_data = ExtraFieldsV2 {
            round: *round,
            quorum_cert: qc,
        }
        .encode_extra()
        .into();
        headers.push(h);
    }
    anchor_at(&spec, &headers, 1, rounds.len() - 2, set)
}

fn masters() -> Vec<SigningKey> {
    (0..9).map(key).collect()
}

fn good_rounds() -> Vec<u64> {
    (0..5).map(|i| R0 + i).collect()
}

fn gap_of_s() -> u64 {
    gap_snapshot::gap_number(S, E, G).unwrap()
}

/// Every QC signed by the first `n` masters.
fn by_first(ms: &[SigningKey], n: usize) -> impl Fn(usize, &B256) -> Vec<Bytes> + '_ {
    move |_, d| ms[..n].iter().map(|k| sign(k, d)).collect()
}

fn good() -> Anchor {
    let ms = masters();
    let signer = by_first(&ms, 7);
    synthetic(&ms, &good_rounds(), gap_of_s(), &signer)
}

fn check(a: &Anchor) -> Result<(), AnchorError> {
    let spec = Network::Apothem.spec();
    let pins = pin(a);
    verify_anchor(&spec, a, &ctx(&pins))
}

#[test]
fn synthetic_good_anchor_verifies() {
    // 0.667 * 9 = 6.003: seven signers pass
    assert_eq!(check(&good()), Ok(()));
}

#[test]
fn wrong_chain_genesis_root_stale_and_version_are_refused() {
    let mut a = good();
    a.chain_id = 50;
    assert_eq!(
        check(&a),
        Err(AnchorError::WrongChain { want: 51, got: 50 })
    );

    let mut a = good();
    let main = Network::Mainnet.spec().genesis_hash;
    a.genesis_hash = main;
    assert_eq!(
        check(&a),
        Err(AnchorError::WrongGenesis {
            want: Network::Apothem.spec().genesis_hash,
            got: main
        })
    );

    let mut a = good();
    let real = a.state_root;
    a.state_root = B256::repeat_byte(5);
    assert_eq!(
        check(&a),
        Err(AnchorError::RootMismatch {
            want: B256::repeat_byte(5),
            got: real
        })
    );

    let spec = Network::Apothem.spec();
    let a = good();
    let pins = pin(&a);
    let late = Context {
        now: a.created_at + DAY + 1,
        ..ctx(&pins)
    };
    assert_eq!(
        verify_anchor(&spec, &a, &late),
        Err(AnchorError::Stale {
            age: DAY + 1,
            max_age: DAY
        })
    );
    // the manifest's own max age is not the limit: the verifier's cap is
    let mut forever = a.clone();
    forever.max_age = u64::MAX;
    let late = Context {
        now: a.created_at + 7 * DAY + 1,
        ..ctx(&pins)
    };
    assert_eq!(
        verify_anchor(&spec, &forever, &late),
        Err(AnchorError::Stale {
            age: 7 * DAY + 1,
            max_age: 7 * DAY
        })
    );
    let early = Context {
        now: a.created_at - 1,
        ..ctx(&pins)
    };
    assert!(matches!(
        verify_anchor(&spec, &a, &early),
        Err(AnchorError::FromFuture { .. })
    ));

    // a later format: refused as such, also when its fields are not those of this one
    let mut a = good();
    a.version = FORMAT_VERSION + 1;
    assert_eq!(
        check(&a),
        Err(AnchorError::UnsupportedFormat {
            found: 2,
            supported: 1
        })
    );
    assert_eq!(
        Anchor::decode(&a.encode()),
        Err(AnchorError::UnsupportedFormat {
            found: 2,
            supported: 1
        })
    );
    let alien = alloy_rlp::encode(vec![2u64, 7, 7]);
    assert_eq!(
        Anchor::decode(&alien),
        Err(AnchorError::UnsupportedFormat {
            found: 2,
            supported: 1
        })
    );
    assert!(matches!(
        Anchor::decode(&[0xc0]),
        Err(AnchorError::Encoding(_))
    ));
}

#[test]
fn forged_under_threshold_duplicate_and_high_s_certificates_are_refused() {
    let ms = masters();
    let build = |s: Signer<'_>| synthetic(&ms, &good_rounds(), gap_of_s(), s);

    // six of nine, one short
    let a = build(&by_first(&ms, 6));
    assert_eq!(
        check(&a),
        Err(AnchorError::UnderThreshold {
            unique: 6,
            required: 7,
            masternodes: 9
        })
    );
    // a signer repeated: counts once
    let dup = |_: usize, d: &B256| {
        let s = sign(&ms[0], d);
        vec![s; 12]
    };
    assert_eq!(
        check(&build(&dup)),
        Err(AnchorError::UnderThreshold {
            unique: 1,
            required: 7,
            masternodes: 9
        })
    );
    // six distinct and each of them twice: six
    let twice = |_: usize, d: &B256| {
        let mut v: Vec<Bytes> = ms[..6].iter().map(|k| sign(k, d)).collect();
        v.extend(ms[..6].iter().map(|k| sign(k, d)));
        v
    };
    assert!(matches!(
        check(&build(&twice)),
        Err(AnchorError::UnderThreshold { unique: 6, .. })
    ));
    // enough signatures, from keys that are not the masternodes
    let outsiders: Vec<SigningKey> = (50..60).map(key).collect();
    let forged = |_: usize, d: &B256| outsiders.iter().map(|k| sign(k, d)).collect();
    assert!(matches!(
        check(&build(&forged)),
        Err(AnchorError::ForgedCertificate(
            V2Error::SignerNotMasternode(_)
        ))
    ));
    // seven good signatures and one that recovers nothing
    let junk = |i: usize, d: &B256| {
        let mut v = by_first(&ms, 7)(i, d);
        v.push(Bytes::from(vec![0u8; 65]));
        v
    };
    assert_eq!(
        check(&build(&junk)),
        Err(AnchorError::ForgedCertificate(
            V2Error::SignatureUnrecoverable
        ))
    );
    // the malleated twin of a good signature (s to n - s): one key cannot count twice
    let high = |i: usize, d: &B256| {
        let mut v = by_first(&ms, 7)(i, d);
        let mut twin = v[0].to_vec();
        let n = U256::from_be_slice(&alloy_primitives::hex!(
            "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141"
        ));
        let s = U256::from_be_slice(&twin[32..64]);
        twin[32..64].copy_from_slice(&(n - s).to_be_bytes::<32>());
        twin[64] ^= 1;
        v.push(twin.into());
        v
    };
    assert_eq!(
        check(&build(&high)),
        Err(AnchorError::ForgedCertificate(V2Error::SignatureHighS))
    );
}

#[test]
fn a_manifest_signed_by_its_own_keys_is_not_pinned() {
    // Everything in it is valid: a full quorum of the masternodes it lists. Nothing ties those to
    // the chain, so without a pin from outside it is refused.
    let a = good();
    let spec = Network::Apothem.spec();
    assert_eq!(
        verify_anchor(&spec, &a, &ctx(&[])),
        Err(AnchorError::NotPinned)
    );
    let other = [(a.number, B256::repeat_byte(3))];
    assert_eq!(
        verify_anchor(&spec, &a, &ctx(&other)),
        Err(AnchorError::NotPinned)
    );
    // pinning the switch header is as good as pinning the checkpoint
    let sw = [(a.provenance.switch_number, a.provenance.switch_hash)];
    assert_eq!(verify_anchor(&spec, &a, &ctx(&sw)), Ok(()));
}

#[test]
fn an_attacker_chain_under_an_honest_pin_is_refused() {
    let honest = good();
    let ms: Vec<SigningKey> = (20..29).map(key).collect();
    let evil = synthetic(&ms, &good_rounds(), gap_of_s(), &by_first(&ms, 9));
    // same pin, the honest checkpoint hash
    let spec = Network::Apothem.spec();
    let pins = pin(&honest);
    // the evil manifest claims the honest checkpoint hash and brings its own headers
    let mut claim = evil.clone();
    claim.hash = honest.hash;
    assert!(matches!(
        verify_anchor(&spec, &claim, &ctx(&pins)),
        Err(AnchorError::CheckpointHash { .. })
    ));
    // or its own hash, which the pin does not name
    assert_eq!(
        verify_anchor(&spec, &evil, &ctx(&pins)),
        Err(AnchorError::NotPinned)
    );
}

#[test]
fn proofs_that_do_not_commit_are_refused() {
    let ms = masters();
    let sign7 = by_first(&ms, 7);
    // a round is skipped above the checkpoint: no 3-chain
    let mut rounds = good_rounds();
    for r in &mut rounds[3..] {
        *r += 1;
    }
    let a = synthetic(&ms, &rounds, gap_of_s(), &sign7);
    assert_eq!(check(&a), Err(AnchorError::NotCommitted));
    // a block opens the next epoch inside the proof
    let mut rounds = good_rounds();
    for r in &mut rounds[4..] {
        *r += E;
    }
    let a = synthetic(&ms, &rounds, gap_of_s(), &sign7);
    assert!(matches!(check(&a), Err(AnchorError::EpochSwitchInProof(_))));
    // the QC names another gap block
    let a = synthetic(&ms, &good_rounds(), gap_of_s() + 1, &sign7);
    assert_eq!(
        check(&a),
        Err(AnchorError::GapNumber {
            want: gap_of_s(),
            got: gap_of_s() + 1
        })
    );
    // a header that does not link
    let mut a = good();
    a.headers[3].parent_hash = B256::repeat_byte(4);
    assert_eq!(check(&a), Err(AnchorError::BrokenChain(S + 3)));
    // a QC that certifies another block than its parent
    let mut a = good();
    let last = a.headers.last_mut().unwrap();
    let mut f = last.extra_fields_v2().unwrap();
    f.quorum_cert.proposed_block_info.hash = B256::repeat_byte(8);
    last.extra_data = f.encode_extra().into();
    assert_eq!(check(&a), Err(AnchorError::QcBlockMismatch(S + 4)));
    // no headers at all
    let mut a = good();
    a.headers.clear();
    assert_eq!(check(&a), Err(AnchorError::EmptyProof));
    // a header that is not an epoch switch listed as the validator header
    let mut a = good();
    a.headers.remove(0);
    a.provenance.switch_number = S + 1;
    a.provenance.switch_hash = a.headers[0].hash();
    let mut b = a.clone();
    b.number = S + 1;
    b.hash = a.headers[0].hash();
    b.state_root = a.headers[0].state_root;
    assert_eq!(check(&b), Err(AnchorError::NotEpochSwitch(S + 1)));
}

// ------------------------------------------------------------------ retention table

fn sorted(mut v: Vec<(Class, u64)>) -> Vec<(Class, u64)> {
    v.sort();
    v
}

fn at(f: ForkBlock) -> u64 {
    match f {
        ForkBlock::At(n) => n,
        other => panic!("{other:?}"),
    }
}

/// Apothem, derived by hand from the table in `spec/retention.md`: E = 900, G = 450, V2 switch
/// 56,828,700, `TIPSigning` 3,000,000, `TIPUpgradePenalty` and `TIPFixedReward` 83,600,000,
/// `limitPenaltyEpoch` 5, protector and observer tiers from round 27,360,000.
fn oracle(rule: Rule, n: u64) -> Vec<(Class, u64)> {
    use Class::*;
    let (sw, up, sig) = (56_828_700u64, 83_600_000u64, 3_000_000u64);
    let v = match rule {
        Rule::V1Reward if n < sw => {
            let mut v = vec![(Header, 1800), (Body, 1799)];
            if n.saturating_sub(1799) < sig {
                v.push((Receipt, 1799));
            }
            v
        }
        Rule::V2Reward if (sw..up).contains(&n) => vec![(Header, 1800), (Body, 1799)],
        Rule::TieredReward if n >= up => vec![(Header, 3149), (Body, 1799), (GapSnapshot, 3149)],
        Rule::V1Penalty if n <= sw && n >= sig => {
            vec![(Header, 4500), (Body, 150), (GapSnapshot, 450)]
        }
        Rule::V2PenaltyPre if n > sw && n < up => {
            let mut v = vec![(Header, 900)];
            if n > sw + 900 {
                v.push((Body, 150));
            }
            v
        }
        Rule::V2PenaltyPost if n >= up => {
            let mut v = vec![(Header, 4500)];
            if n > sw + 900 {
                v.push((Body, 900));
            }
            v
        }
        Rule::GapPool if n > sw => vec![(Header, 1349), (GapSnapshot, 1349)],
        Rule::V1Snapshot if n <= sw => vec![(Header, 900), (GapSnapshot, 900)],
        Rule::QcEpoch if n > sw => vec![(Header, 900)],
        Rule::ThreeChain if n > sw + 2 => vec![(Header, 3)],
        Rule::TcWalk if n > sw => vec![(Header, 1800), (GapSnapshot, 3149)],
        _ => vec![],
    };
    sorted(v)
}

/// Every block at which the Apothem fork table, the effective switches, the V2 switch or a
/// guard of a rule changes something, and its two neighbours.
fn apothem_discontinuities(spec: &ChainSpec) -> BTreeSet<u64> {
    let mut at_blocks: Vec<u64> = spec
        .fork_rows()
        .into_iter()
        .filter_map(|(_, f)| match f {
            ForkBlock::At(n) => Some(n),
            _ => None,
        })
        .collect();
    let e = &spec.effective;
    for f in [
        e.constantinople,
        e.eip1559,
        e.gas50x,
        e.istanbul,
        e.petersburg,
        e.tip2019,
        e.tip_no_halving_mn_reward,
        e.tip_randomize,
        e.tip_signing,
        e.tip_trc21_fee,
        e.tip_xdcx,
        e.tip_xdcx_cancellation_fee,
        e.tip_xdcx_miner_disable,
        e.tip_xdcx_receiver_disable,
    ] {
        if let ForkBlock::At(n) = f {
            at_blocks.push(n);
        }
    }
    let sw = spec.xdpos.v2.as_ref().unwrap().switch_block;
    at_blocks.extend([
        sw,
        sw + 2,           // the 3-chain starts above it
        sw + 900,         // the comeback reads start above one epoch
        3_000_000 + 1799, // a V1 window that starts at TIPSigning needs no receipts
    ]);
    at_blocks
        .into_iter()
        .flat_map(|b| [b.saturating_sub(1), b, b + 1])
        .collect()
}

#[test]
fn every_rule_lookback_matches_the_hand_derivation_at_every_apothem_discontinuity() {
    let spec = Network::Apothem.spec();
    let points = apothem_discontinuities(&spec);
    assert!(points.len() > 40, "{}", points.len());
    let all: Vec<Rule> = Rule::CONSENSUS.into_iter().collect();
    let mut seen: BTreeMap<Rule, usize> = BTreeMap::new();
    for &n in &points {
        for &rule in &all {
            let got = sorted(rule.lookback(&spec, n).unwrap());
            assert_eq!(got, oracle(rule, n), "{rule:?} at {n}");
            if !got.is_empty() {
                *seen.entry(rule).or_default() += 1;
            }
        }
    }
    // every rule is exercised on at least one point
    assert_eq!(seen.len(), all.len(), "{seen:?}");
}

#[test]
fn the_window_is_derived_from_the_spec_not_a_constant() {
    let apothem = Network::Apothem.spec();
    let mainnet = Network::Mainnet.spec();
    let win = |spec: &ChainSpec, pivot: u64| {
        let r = Retention::derive(spec, pivot).unwrap();
        (
            r.window(Class::Header),
            r.window(Class::Body),
            r.window(Class::Receipt),
            r.window(Class::GapSnapshot),
        )
    };
    // Apothem after TIPUpgradePenalty: five epochs of headers (limitPenaltyEpoch 5 * 900), the
    // reward bodies of two, the pool of the standby tiers 3 * 900 + 450 - 1 back.
    assert_eq!(
        win(&apothem, 84_000_000),
        (
            (4500, Some(Rule::V2PenaltyPost)),
            (1799, Some(Rule::TieredReward)),
            (0, None),
            (3149, Some(Rule::TieredReward))
        )
    );
    // Apothem before it: two epochs of headers and bodies, set by the reward window; the gap
    // snapshots reach back further, for the timeout certificate of the previous epoch
    assert_eq!(
        win(&apothem, 70_000_000),
        (
            (1800, Some(Rule::V2Reward)),
            (1799, Some(Rule::V2Reward)),
            (0, None),
            (3149, Some(Rule::TcWalk))
        )
    );
    // mainnet now (no TIPUpgradePenalty, no TIPFixedReward): the same
    assert_eq!(
        win(&mainnet, 100_000_000),
        (
            (1800, Some(Rule::V2Reward)),
            (1799, Some(Rule::V2Reward)),
            (0, None),
            (3149, Some(Rule::TcWalk))
        )
    );
    // the day mainnet schedules the upgrade with limitPenaltyEpoch 5, two epochs are not enough
    let mut later = mainnet.clone();
    later.tip_upgrade_penalty = ForkBlock::At(101_000_000);
    later.tip_fixed_reward = ForkBlock::At(101_000_000);
    {
        let v2 = later.xdpos.v2.as_mut().unwrap();
        for c in v2.configs.values_mut() {
            c.limit_penalty_epoch = 5;
            c.max_protector_nodes = 3;
        }
    }
    assert_eq!(win(&later, 101_000_000).0 .0, 4500);
    assert_eq!(win(&later, 101_000_000).3 .0, 3149);
    // a window of two epochs would refuse a rule that reads five
    let fixed = 2 * 900;
    assert!(win(&apothem, 84_000_000).0 .0 > fixed);
    // the epoch is a parameter too
    let mut short = apothem.clone();
    short.xdpos.epoch = 450;
    short.xdpos.gap = 225;
    assert_eq!(win(&short, 84_000_000).0 .0, 5 * 450);
    // pivots below TIPSigning keep the receipts of the V1 reward window
    let r = Retention::derive(&apothem, 2_000_000).unwrap();
    assert_eq!(r.window(Class::Receipt), (1799, Some(Rule::V1Reward)));
    assert_eq!(r.is_within(Class::Receipt, 2_000_001 - 1799), Ok(()));
    assert!(r.is_within(Class::Receipt, 2_000_001 - 1800).is_err());
}

#[test]
fn the_window_grows_at_the_upgrade_and_not_at_the_proportional_row() {
    let spec = Network::Apothem.spec();
    let up = at(spec.tip_upgrade_penalty);
    let prop = at(spec.tip_proportional_reward);
    assert_eq!((up, prop), (83_600_000, 84_733_000));
    let headers = |pivot: u64| {
        Retention::derive(&spec, pivot)
            .unwrap()
            .window(Class::Header)
            .0
    };
    // a block after the pivot is on the new rule when pivot + epoch >= up
    assert_eq!(headers(up - E - 1), 1800);
    assert_eq!(headers(up - E), 4500 - (E - 1)); // only the last block of the horizon
    assert_eq!(headers(up - 1), 4500);
    assert_eq!(headers(up), 4500);
    // TIPProportionalReward changes the sum, not the data it reads
    for p in [prop - E - 1, prop - 1, prop, prop + 1] {
        let (a, b) = (
            Retention::derive(&spec, p).unwrap(),
            Retention::derive(&spec, prop - E - 1).unwrap(),
        );
        for c in [
            Class::Header,
            Class::Body,
            Class::Receipt,
            Class::GapSnapshot,
        ] {
            assert_eq!(a.window(c), b.window(c), "{c:?} at {p}");
        }
    }
}

/// The empirical side: 2,533 consecutive Apothem epoch switches from the archive
/// (`XDPoS_getEpochNumbersBetween`, 83,000,425 to 85,245,161).
#[test]
fn real_epoch_spacing_stays_inside_the_derived_bounds() {
    let sw: Vec<u64> =
        serde_json::from_str(&fixture("apothem-epoch-switches-83000425-85245161.json")).unwrap();
    assert_eq!(sw.len(), 2533);
    let span = |k: usize| sw.windows(k + 1).map(|w| w[k] - w[0]).max().unwrap();
    // one epoch is at most E blocks, k epochs at most k * E, and the bound is reached
    assert_eq!(span(1), E);
    assert_eq!(span(2), 2 * E);
    assert_eq!(span(5), 5 * E);
    assert!(sw.windows(2).map(|w| w[1] - w[0]).min().unwrap() < E);
}

#[test]
fn the_derived_window_covers_every_rule_on_the_first_epoch_after_a_pivot() {
    let apothem = Network::Apothem.spec();
    let mainnet = Network::Mainnet.spec();
    let sw = apothem.xdpos.v2.as_ref().unwrap().switch_block;
    let pivots = [
        (&apothem, sw),
        (&apothem, sw - 1),
        (&apothem, sw + 1),
        (&apothem, 3_000_000),
        (&apothem, 83_600_000),
        (&apothem, 83_599_999),
        (&apothem, 84_733_000),
        (&apothem, 85_000_050), // a gap block: 85,000,050 % 900 == 450
        (&mainnet, 80_370_000),
        (&mainnet, 100_000_000),
    ];
    for (spec, p) in pivots {
        let r = Retention::derive(spec, p).unwrap();
        for j in 0..E {
            let n = p + 1 + j;
            for rule in Rule::CONSENSUS {
                for (class, l) in rule.lookback(spec, n).unwrap() {
                    assert_eq!(
                        r.is_within(class, n.saturating_sub(l)),
                        Ok(()),
                        "{rule:?} at {n} reads {class:?} to {}",
                        n - l
                    );
                }
            }
        }
        // and the window is no larger than the largest need: its floor is a need
        for class in [Class::Header, Class::Body, Class::GapSnapshot] {
            let (w, rule) = r.window(class);
            if let Some(rule) = rule {
                let max_need = (0..E)
                    .flat_map(|j| {
                        Rule::CONSENSUS.into_iter().flat_map(move |rl| {
                            rl.lookback(spec, p + 1 + j)
                                .unwrap()
                                .into_iter()
                                .filter(move |(c, _)| *c == class)
                                .map(move |(_, l)| l.saturating_sub(j))
                        })
                    })
                    .max()
                    .unwrap();
                assert_eq!(w, max_need, "{class:?} at {p}");
                assert!(rule
                    .lookback(spec, p + 1)
                    .unwrap()
                    .iter()
                    .all(|(_, l)| *l > 0));
            }
        }
    }
}

// ------------------------------------------------------------------ the pivot plan

fn range(plan: &xone_sync::bootstrap::Plan, c: Class) -> Option<(u64, u64)> {
    plan.of(c).next().map(|n| (n.first, n.last))
}

fn gaps(plan: &xone_sync::bootstrap::Plan) -> Vec<u64> {
    plan.of(Class::GapSnapshot).map(|n| n.first).collect()
}

#[test]
fn plan_at_the_upgrade_lists_the_first_post_pivot_reward_inputs() {
    let spec = Network::Apothem.spec();
    let p = 83_600_000;
    let plan = plan_at(&spec, p).unwrap();
    // block p + 1 can be the switch that pays and penalises: it reads five epochs of headers
    // (the `Penalties` of the last five switches), two of bodies (the reward window, the sign
    // transactions of the signed blocks), and the pool of the epochs that the tier sets come from.
    assert_eq!(range(&plan, Class::Header), Some((p + 1 - 4500, p)));
    assert_eq!(range(&plan, Class::Body), Some((p + 1 - 1799, p)));
    assert_eq!(range(&plan, Class::Receipt), None);
    assert_eq!(range(&plan, Class::State), Some((p, p)));
    // gap blocks k * 900 - 450 from p + 1 - 3149 = 83,596,852 up to p
    assert_eq!(
        gaps(&plan),
        vec![83_596_950, 83_597_850, 83_598_750, 83_599_650]
    );
    assert_eq!(
        plan.of(Class::Header).next().unwrap().rule,
        Some(Rule::V2PenaltyPost)
    );
    assert_eq!(
        plan.of(Class::Body).next().unwrap().rule,
        Some(Rule::TieredReward)
    );
}

#[test]
fn plan_at_the_proportional_row_reads_what_the_tiered_plan_reads() {
    let spec = Network::Apothem.spec();
    let p = 84_733_000;
    let plan = plan_at(&spec, p).unwrap();
    assert_eq!(range(&plan, Class::Header), Some((p + 1 - 4500, p)));
    assert_eq!(range(&plan, Class::Body), Some((p + 1 - 1799, p)));
    // gap blocks k * 900 - 450 from p + 1 - 3149 = 84,729,852 up to p
    assert_eq!(
        gaps(&plan),
        vec![84_730_050, 84_730_950, 84_731_850, 84_732_750]
    );
    // the same arithmetic as one block earlier: the row adds no class and no length
    let before = plan_at(&spec, p - 1).unwrap();
    assert_eq!(
        before
            .needs
            .iter()
            .map(|n| (n.class, n.last - n.first))
            .collect::<Vec<_>>(),
        plan.needs
            .iter()
            .map(|n| (n.class, n.last - n.first))
            .collect::<Vec<_>>()
    );
}

#[test]
fn plan_at_a_gap_block_holds_that_gap_snapshot() {
    let spec = Network::Apothem.spec();
    let p = 85_000_050;
    assert_eq!(p % E, E - G);
    let plan = plan_at(&spec, p).unwrap();
    // the gap block of the pivot itself is in the plan: its pool comes from the state of the
    // pivot, which the node has, but the next epoch's switch reads it from the snapshot
    assert_eq!(gaps(&plan).last(), Some(&p));
    assert_eq!(
        gaps(&plan),
        vec![84_997_350, 84_998_250, 84_999_150, 85_000_050]
    );
    assert_eq!(range(&plan, Class::Header), Some((p + 1 - 4500, p)));
}

#[test]
fn plan_at_the_v2_switch_reaches_into_v1() {
    let spec = Network::Apothem.spec();
    let sw = 56_828_700;
    // pivot on the switch block: the first V2 reward window opens on the V1 checkpoint 900 below
    let plan = plan_at(&spec, sw).unwrap();
    let (h, _) = range(&plan, Class::Header).unwrap();
    let (b, _) = range(&plan, Class::Body).unwrap();
    assert!(h <= sw - 900 && b <= sw - 899, "{h} {b}");
    // V1 receipts are not needed: TIPSigning is far below
    assert_eq!(range(&plan, Class::Receipt), None);
    // the first V2 epoch's pool is the list of the V1 switch block: that header is held
    assert!(h <= sw);
    // pivot one below it, in V1: the V1 reward window and the V1 snapshot walk
    let v1 = plan_at(&spec, sw - 1).unwrap();
    assert_eq!(range(&v1, Class::Body), Some((sw - 1799, sw - 1)));
    assert!(range(&v1, Class::Header).unwrap().0 <= sw - 1800);
    // V1 penalties reach 4,500 back for the comeback `Penalties`
    assert_eq!(
        v1.of(Class::Header).next().unwrap().rule,
        Some(Rule::V1Penalty)
    );
    assert_eq!(range(&v1, Class::Header), Some((sw - 4500, sw - 1)));
}

#[test]
fn plan_below_tipsigning_keeps_the_receipts_of_the_reward_window() {
    let spec = Network::Apothem.spec();
    // a V1 reward at 3,000,500 pays blocks 2,999,701 ..= 3,000,400 by sign transactions whose
    // receipts decide; blocks below 3,000,000 need them
    let p = 3_000_499;
    let plan = plan_at(&spec, p).unwrap();
    assert_eq!(range(&plan, Class::Receipt), Some((p + 1 - 1799, p)));
    assert_eq!(
        plan.of(Class::Receipt).next().unwrap().rule,
        Some(Rule::V1Reward)
    );
    // from p = 3,001,798 the window starts at TIPSigning: none
    assert_eq!(
        range(&plan_at(&spec, 3_001_900).unwrap(), Class::Receipt),
        None
    );
}

#[test]
fn plan_of_a_real_anchor_is_the_plan_at_its_number() {
    let (spec, a) = real_v2();
    let plan = pivot_plan(&spec, &a).unwrap();
    assert_eq!(plan, plan_at(&spec, 84_954_831).unwrap());
    assert_eq!(plan.pivot, a.number);
}

#[test]
fn mainnet_needs_two_epochs_of_blocks_today() {
    let spec = Network::Mainnet.spec();
    let plan = plan_at(&spec, 100_000_000).unwrap();
    assert_eq!(
        range(&plan, Class::Header),
        Some((100_000_001 - 1800, 100_000_000))
    );
    assert_eq!(
        range(&plan, Class::Body),
        Some((100_000_001 - 1799, 100_000_000))
    );
    // the gap snapshots reach 3,149 back (the TC of the previous epoch), from 99,996,852
    assert_eq!(gaps(&plan), vec![99_997_650, 99_998_550, 99_999_450]);
}

// ------------------------------------------------------------------ typed error, never empty

#[test]
fn outside_the_window_is_the_typed_error_for_every_rule() {
    let apothem = Network::Apothem.spec();
    // every rule, on a block that runs it
    let cases = [
        (Rule::V1Reward, 20_000_000u64),
        (Rule::V2Reward, 70_000_000),
        (Rule::TieredReward, 84_000_000),
        (Rule::V1Penalty, 20_000_000),
        (Rule::V2PenaltyPre, 70_000_000),
        (Rule::V2PenaltyPost, 84_000_000),
        (Rule::GapPool, 84_000_000),
        (Rule::V1Snapshot, 20_000_000),
        (Rule::QcEpoch, 84_000_000),
        (Rule::ThreeChain, 84_000_000),
        (Rule::TcWalk, 84_000_000),
        (Rule::RpcMasternodes, 84_000_000),
        (Rule::RpcMissedRounds, 84_000_000),
        (Rule::RpcGetLogs, 84_000_000),
    ];
    let mut covered = BTreeSet::new();
    for (rule, n) in cases {
        covered.insert(rule);
        let reads = rule.lookback(&apothem, n).unwrap();
        assert!(!reads.is_empty(), "{rule:?} does not run at {n}");
        for (class, l) in reads {
            // a node whose pivot is n - 1 and that keeps exactly this rule's window
            let r = Retention::of_rule(&apothem, rule, n).unwrap();
            let first = n - l;
            assert_eq!(r.is_within(class, first), Ok(()), "{rule:?} {class:?}");
            assert_eq!(
                r.is_within(class, first - 1),
                Err(Missing::UnsupportedHistory {
                    class,
                    block: first - 1,
                    floor: first,
                    pivot: n - 1
                }),
                "{rule:?} {class:?}"
            );
        }
    }
    assert_eq!(covered.len(), Rule::CONSENSUS.len() + Rule::RPC.len());
}

#[test]
fn requests_below_the_floors_are_refused_and_never_empty() {
    let spec = Network::Apothem.spec();
    let p = 84_000_000;
    let r = Retention::derive(&spec, p).unwrap();
    // eth_getLogs: no receipts at or below the pivot on a V2 pivot
    assert_eq!(r.serves(&spec, Rule::RpcGetLogs, p + 1), Ok(()));
    assert_eq!(
        r.serves(&spec, Rule::RpcGetLogs, p),
        Err(Error::Missing(Missing::UnsupportedHistory {
            class: Class::Receipt,
            block: p,
            floor: p + 1,
            pivot: p
        }))
    );
    // XDPoS_getMasternodesByNumber reads 2E + G - 2 = 2,248 headers and gap snapshots below the
    // block; the gap snapshots are kept from p + 1 - 3,149, so it is served from p - 900 up
    assert_eq!(r.serves(&spec, Rule::RpcMasternodes, p - 900), Ok(()));
    assert_eq!(
        r.serves(&spec, Rule::RpcMasternodes, p - 901),
        Err(Error::Missing(Missing::UnsupportedHistory {
            class: Class::GapSnapshot,
            block: p - 901 - 2248,
            floor: p + 1 - 3149,
            pivot: p
        }))
    );
    // getMissedRoundsInEpochByBlockNum reads E = 900 headers, kept from p + 1 - 4,500
    assert_eq!(r.serves(&spec, Rule::RpcMissedRounds, p - 3599), Ok(()));
    assert!(matches!(
        r.serves(&spec, Rule::RpcMissedRounds, p - 3600),
        Err(Error::Missing(Missing::UnsupportedHistory {
            class: Class::Header,
            ..
        }))
    ));
    // the state of a block below the pivot
    assert!(r.is_within(Class::State, p - 1).is_err());
    assert_eq!(r.is_within(Class::State, p), Ok(()));
}

// ------------------------------------------------------------------ the engines read what the table says

fn hash_of(n: u64) -> B256 {
    B256::from(U256::from(n + 1))
}

fn number_of(h: B256) -> u64 {
    u64::try_from(U256::from_be_bytes(h.0) - U256::from(1)).unwrap()
}

/// A canonical chain with epoch switches at `switches`, which holds headers from `floor_h` up and
/// bodies from `floor_b` up, and says `None` for the rest as a pruned database would.
struct Chain {
    floor_h: u64,
    floor_b: u64,
    switches: Vec<u64>,
}

impl BlockReader for Chain {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        (number >= self.floor_h && hash == hash_of(number)).then(|| HeaderView {
            number,
            hash,
            parent_hash: hash_of(number.saturating_sub(1)),
            miner: Address::repeat_byte(1),
            epoch_switch: self.switches.contains(&number),
        })
    }
    fn sign_txs(&self, _hash: B256, number: u64) -> Option<Vec<SignTx>> {
        (number >= self.floor_b).then(Vec::new)
    }
}

impl PenaltyHistory for Chain {
    fn penalties(&self, hash: B256, back: usize) -> Option<Vec<Address>> {
        let n = number_of(hash);
        let mut below: Vec<u64> = self.switches.iter().copied().filter(|s| *s <= n).collect();
        below.sort_unstable_by(|a, b| b.cmp(a));
        let s = *below.get(back)?;
        // a node that was penalised and is a candidate again: the comeback scan has work to do
        (s >= self.floor_h).then(|| vec![Address::repeat_byte(2)])
    }
}

fn is_missing(e: &ChainError) -> bool {
    matches!(e, ChainError::Missing { .. })
}

#[test]
fn reward_and_penalty_engines_read_exactly_the_table_floors() {
    let spec = Network::Apothem.spec();
    // V2 flat reward at n: switches 900 and 1800 below
    let n = 70_000_000;
    let r = Retention::of_rule(&spec, Rule::V2Reward, n).unwrap();
    let chain = |h: u64, b: u64| Chain {
        floor_h: h,
        floor_b: b,
        switches: vec![n - 900, n - 1800],
    };
    let ok = chain(r.floor(Class::Header), r.floor(Class::Body));
    let located = reward::v2::locate(&spec, &ok, n, hash_of(n - 1)).unwrap();
    assert_eq!((located.start, located.end), (n - 1799, n - 901));
    let short = chain(r.floor(Class::Header) + 1, r.floor(Class::Body));
    assert!(is_missing(
        &reward::v2::locate(&spec, &short, n, hash_of(n - 1)).unwrap_err()
    ));
    // the window blocks themselves: bodies of start ..= n - 1
    let counter = SignTxCounter::new(&ok);
    assert!(counter
        .collect((hash_of(n - 1), n - 1), located.start)
        .is_ok());
    let no_bodies = chain(r.floor(Class::Header), r.floor(Class::Body) + 1);
    assert!(is_missing(
        &SignTxCounter::new(&no_bodies)
            .collect((hash_of(n - 1), n - 1), located.start)
            .unwrap_err()
    ));

    // V1 reward: 2r bodies below a checkpoint, the checkpoint header at n - 2r
    let n = 20_000_700u64;
    let r = Retention::of_rule(&spec, Rule::V1Reward, n).unwrap();
    let located = reward::v1::locate(&spec, n).unwrap();
    assert_eq!(located.checkpoint, n - r.window(Class::Header).0);
    assert_eq!(located.start, n - r.window(Class::Body).0);
    let v1 = Chain {
        floor_h: located.checkpoint,
        floor_b: r.floor(Class::Body),
        switches: vec![],
    };
    assert!(SignTxCounter::new(&v1)
        .collect((hash_of(n - 1), n - 1), located.start)
        .is_ok());
    let v1_short = Chain {
        floor_b: located.start + 1,
        ..v1
    };
    assert!(is_missing(
        &SignTxCounter::new(&v1_short)
            .collect((hash_of(n - 1), n - 1), located.start)
            .unwrap_err()
    ));
}

fn v2_input<'a>(n: u64, masters: &'a [Address], pool: &'a [Address]) -> penalty::v2::Input<'a> {
    penalty::v2::Input {
        number: n,
        parent: hash_of(n - 1),
        round: 28_000_000,
        parent_masternodes: masters,
        candidates: pool,
    }
}

#[test]
fn v2_penalty_engine_reads_the_headers_and_bodies_the_table_says() {
    let spec = Network::Apothem.spec();
    let masters = [Address::repeat_byte(1)];
    let pool = [Address::repeat_byte(1), Address::repeat_byte(2)];
    for (rule, n) in [
        (Rule::V2PenaltyPre, 70_000_000u64),
        (Rule::V2PenaltyPost, 84_000_000),
    ] {
        let r = Retention::of_rule(&spec, rule, n).unwrap();
        let (hw, bw) = (r.window(Class::Header).0, r.window(Class::Body).0);
        let switches: Vec<u64> = (1..=6).map(|k| n - 900 * k).collect();
        let chain = |h: u64, b: u64| Chain {
            floor_h: h,
            floor_b: b,
            switches: switches.clone(),
        };
        let input = v2_input(n, &masters, &pool);
        let ok = chain(n - hw, n - bw);
        assert!(
            penalty::v2::penalties(&spec, &ok, &input).is_ok(),
            "{rule:?}"
        );
        // one header less: the history walk or the epoch walk is short
        let no_h = chain(n - hw + 1, n - bw);
        let e = penalty::v2::penalties(&spec, &no_h, &input).unwrap_err();
        assert!(is_missing(&e), "{rule:?}: {e}");
        // one body less
        let no_b = chain(n - hw, n - bw + 1);
        let e = penalty::v2::penalties(&spec, &no_b, &input).unwrap_err();
        assert!(is_missing(&e), "{rule:?}: {e}");
    }
}

#[test]
fn v1_penalty_engine_and_the_comeback_header_follow_the_table() {
    let spec = Network::Apothem.spec();
    let n = 20_000_700u64;
    let r = Retention::of_rule(&spec, Rule::V1Penalty, n).unwrap();
    // the header whose `Penalties` feed the comeback is n - 5 * 900
    assert_eq!(
        penalty::v1::comeback_epoch(&spec, n),
        Some(n - r.window(Class::Header).0)
    );
    let signers = [Address::repeat_byte(1), Address::repeat_byte(2)];
    let comeback = [Address::repeat_byte(2)];
    let input = penalty::v1::Input {
        number: n,
        parent: hash_of(n - 1),
        candidates: &signers,
        prev_masternodes: &signers,
        comeback_penalties: Some(&comeback),
    };
    // headers n - 899 ..= n - 1 are read by the engine, bodies n - 150 ..= n - 1
    let chain = |h: u64, b: u64| Chain {
        floor_h: h,
        floor_b: b,
        switches: vec![],
    };
    assert!(penalty::v1::penalties(&spec, &chain(n - 899, n - 150), &input).is_ok());
    assert!(is_missing(
        &penalty::v1::penalties(&spec, &chain(n - 898, n - 150), &input).unwrap_err()
    ));
    assert!(is_missing(
        &penalty::v1::penalties(&spec, &chain(n - 899, n - 149), &input).unwrap_err()
    ));
    // the caller does not hold the comeback header: a typed error, not an empty list
    let none = penalty::v1::Input {
        comeback_penalties: None,
        ..input
    };
    assert!(is_missing(
        &penalty::v1::penalties(&spec, &chain(n - 899, n - 150), &none).unwrap_err()
    ));
}

struct NoState;
impl StateRead for NoState {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, _: Address, _: B256) -> U256 {
        U256::ZERO
    }
}

struct Links(BTreeMap<B256, Link>);
impl Headers for Links {
    fn link(&self, hash: B256) -> Option<Link> {
        self.0.get(&hash).copied()
    }
}

#[test]
fn gap_snapshot_and_three_chain_refuse_what_is_absent() {
    // a gap block whose state the node does not hold: the pool is Missing, not an empty pool
    assert!(matches!(
        gap_snapshot::build(&NoState, 84_999_550),
        Err(V2Error::Missing {
            what: "0x88 candidates",
            number: 84_999_550
        })
    ));
    // the 3-chain needs the parent and the grandparent: three blocks back
    let spec = Network::Apothem.spec();
    let sw = spec.xdpos.v2.as_ref().unwrap().switch_block;
    let n = 84_000_000;
    let r = Retention::of_rule(&spec, Rule::ThreeChain, n).unwrap();
    assert_eq!(r.window(Class::Header).0, 3);
    let link = |k: u64| Link {
        number: n - k,
        round: 10 - k,
        hash: hash_of(n - k),
        parent: hash_of(n - k - 1),
    };
    let all = Links(
        [link(0), link(1), link(2)]
            .into_iter()
            .map(|l| (l.hash, l))
            .collect(),
    );
    let got = commit_blocks(&all, sw, None, &link(0)).unwrap().unwrap();
    assert_eq!(got.number, n - 2);
    let no_grand = Links(
        [link(0), link(1)]
            .into_iter()
            .map(|l| (l.hash, l))
            .collect(),
    );
    assert!(matches!(
        commit_blocks(&no_grand, sw, None, &link(0)),
        Err(V2Error::Missing {
            what: "grandparent",
            ..
        })
    ));
    let none = Links(BTreeMap::new());
    assert!(matches!(
        commit_blocks(&none, sw, None, &link(0)),
        Err(V2Error::Missing { what: "parent", .. })
    ));
}
