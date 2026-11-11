#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! XDPoS V1 against the Apothem archive and the pinned geth-XDC. The vectors in
//! `conformance/vectors/consensus/v1/` are written by `crates/xone-consensus/oracle/v1`; every
//! expected value in them is a header field of the archive or a value the pinned geth computed.
//! Nothing here is typed by hand except the two Apothem constants named below.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::Deserialize;
use xone_chainspec::Network;
use xone_consensus::v1::{
    checkpoint::{
        checkpoint_masternodes, checkpoint_penalties, masternodes_of, max_masternodes,
        select_candidates, verify_checkpoint_list,
    },
    double_validation::{assigned_validator, check_double_validation},
    penalty::{penalties_v1, penalty_era, PenaltyEra},
    snapshot::{Snapshot, Tally, Vote},
    switch::check_switch_block,
    turn::{authorize, check_difficulty, difficulty},
};
use xone_primitives::{ecrecover, Header};
use xone_state::system_contracts::{signer_0x89, validator_0x88};
use xone_state::{Account, StateRead};

/// `apothemConstants.TIPIncreaseMasternodes` and `.MaxMasternodesV2`
/// (`consensus/XDPoS/network_constants.go:85`, `:100`). The chain spec carries neither: the fork
/// row `tipIncreaseMasternodesBlock` is unset on Apothem and `XDPoS.maxMasternodesV2` is 0, so geth
/// reads the network constants (`xdpos.go:2163-2170`).
const APOTHEM_TIP_INCREASE: u64 = 5_000_000;
const APOTHEM_MAX_MASTERNODES_V2: u64 = 15;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors/consensus/v1")
}

fn lines<T: for<'de> Deserialize<'de>>(file: &str) -> Vec<T> {
    let path = dir().join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{file}: {e}")))
        .collect()
}

fn header_of(b: &Bytes) -> Header {
    alloy_rlp::decode_exact(b).unwrap()
}

// ------------------------------------------------------------------ the frozen sort

#[derive(Deserialize)]
struct SortCase {
    caps: Vec<u64>,
    order: Vec<usize>,
}

#[test]
fn the_frozen_sort_orders_equal_caps_like_geth() {
    let cases: Vec<SortCase> = lines("gosort.jsonl");
    assert!(cases.len() >= 1_000);
    let mut ties = 0;
    for c in &cases {
        // Equal caps are distinct addresses: the address byte is the original index.
        let cands: Vec<(Address, U256)> = c
            .caps
            .iter()
            .enumerate()
            .map(|(i, cap)| {
                (
                    Address::from_word(B256::from(U256::from(i))),
                    U256::from(*cap),
                )
            })
            .collect();
        let got = if cands.is_empty() {
            vec![]
        } else {
            select_candidates(&cands, u64::MAX).unwrap()
        };
        let got: Vec<usize> = got
            .iter()
            .map(|a| usize::from(a.0[18]) * 256 + usize::from(a.0[19]))
            .collect();
        assert_eq!(got, c.order, "caps {:?}", c.caps);
        ties += usize::from(
            c.caps.len() > 1 && c.caps.iter().collect::<BTreeSet<_>>().len() < c.caps.len(),
        );
    }
    println!(
        "gosort: {} cases, {ties} with equal caps, all equal to geth",
        cases.len()
    );
}

// ------------------------------------------------------------------ snapshot.apply

#[derive(Deserialize)]
struct SnapJson {
    number: u64,
    hash: B256,
    signers: BTreeMap<Address, serde_json::Value>,
    recents: BTreeMap<u64, Address>,
    votes: Option<Vec<VoteJson>>,
    tally: BTreeMap<Address, TallyJson>,
}
#[derive(Deserialize)]
struct VoteJson {
    signer: Address,
    block: u64,
    address: Address,
    authorize: bool,
}
#[derive(Deserialize)]
struct TallyJson {
    authorize: bool,
    votes: usize,
}

impl From<SnapJson> for Snapshot {
    fn from(j: SnapJson) -> Self {
        Snapshot {
            number: j.number,
            hash: j.hash,
            signers: j.signers.into_keys().collect(),
            recents: j.recents,
            votes: j
                .votes
                .unwrap_or_default()
                .into_iter()
                .map(|v| Vote {
                    signer: v.signer,
                    block: v.block,
                    address: v.address,
                    authorize: v.authorize,
                })
                .collect(),
            tally: j
                .tally
                .into_iter()
                .map(|(a, t)| {
                    (
                        a,
                        Tally {
                            authorize: t.authorize,
                            votes: t.votes,
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Window {
    start: SnapJson,
    end: SnapJson,
    headers: Vec<Bytes>,
    why: String,
}

#[test]
fn apply_reproduces_the_archive_snapshot_across_windows() {
    let windows: Vec<Window> = lines("snapshot.jsonl");
    assert!(windows.len() >= 29);
    let (mut blocks, mut crossing) = (0, 0);
    for w in windows {
        let headers: Vec<Header> = w.headers.iter().map(header_of).collect();
        blocks += headers.len();
        crossing += usize::from(headers.iter().any(|h| h.number % 900 == 0));
        let start = Snapshot::from(w.start);
        let want = Snapshot::from(w.end);
        let got = start.apply(&headers, 900).unwrap();
        assert_eq!(
            got, want,
            "window {} {}..{}",
            w.why, start.number, want.number
        );
        // The window as one call and block by block agree.
        let mut step = start.clone();
        for h in &headers {
            step = step.apply(std::slice::from_ref(h), 900).unwrap();
        }
        assert_eq!(step, want);
    }
    println!(
        "snapshot: {blocks} blocks, {crossing} windows over a checkpoint, all equal to the archive"
    );
}

#[derive(Deserialize)]
struct VoteScenario {
    genesis: Bytes,
    headers: Vec<Bytes>,
    at: Vec<usize>,
    snapshots: Vec<SnapJson>,
}

/// Synthetic chains with votes in their headers, epoch 50 (the votes reset at block 50); the snapshots are what the pinned geth computes
/// (the archive's windows hold no vote).
#[test]
fn apply_counts_votes_like_geth() {
    let scenarios: Vec<VoteScenario> = lines("snapvotes.jsonl");
    let (mut checked, mut with_votes, mut changed) = (0, 0, 0);
    for s in scenarios {
        let genesis = header_of(&s.genesis);
        let first = Snapshot::new(
            0,
            genesis.hash(),
            masternodes_of(&genesis.extra_data).unwrap(),
        );
        let headers: Vec<Header> = s.headers.iter().map(header_of).collect();
        for (at, want) in s.at.iter().zip(s.snapshots) {
            let want = Snapshot::from(want);
            let got = first.apply(&headers[..*at], 50).unwrap();
            assert_eq!(got, want, "height {at}");
            checked += 1;
            with_votes += usize::from(!got.votes.is_empty());
            changed += usize::from(got.signers != first.signers);
        }
    }
    println!("snapvotes: {checked} snapshots, {with_votes} with votes in flight, {changed} with a changed signer set");
    assert!(with_votes > 50 && changed > 20);
}

// ------------------------------------------------------------------ checkpoint masternodes

#[derive(Deserialize)]
struct NumPen {
    number: u64,
    penalties: Vec<Address>,
}
#[derive(Deserialize)]
struct NumHash {
    hash: B256,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cp {
    number: u64,
    extra: Vec<Address>,
    penalties: Vec<Address>,
    tip_signing: bool,
    tip_randomize: bool,
    prev_masternodes: Vec<Address>,
    prev_penalties: Vec<NumPen>,
    comeback: Option<NumPen>,
    gap_number: u64,
    candidate_words: Vec<(B256, B256)>,
    snapshot_gap: Vec<Address>,
    #[serde(default)]
    signer_blocks: Vec<NumHash>,
    #[serde(default)]
    signer_words: Vec<(B256, B256)>,
    #[serde(default)]
    creators: Vec<(Address, u64)>,
}

/// The contract words the oracle read, as a state: what the xone-state readers read.
struct Words(HashMap<(Address, B256), U256>);
impl StateRead for Words {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, address: Address, slot: B256) -> U256 {
        self.0.get(&(address, slot)).copied().unwrap_or(U256::ZERO)
    }
}

fn is_subsequence(small: &[Address], big: &[Address]) -> bool {
    let mut it = big.iter();
    small.iter().all(|s| it.any(|b| b == s))
}

#[test]
fn every_checkpoint_list_equals_the_one_derived_from_the_candidates() {
    let spec = Network::Apothem.spec();
    let switch = spec.xdpos.v2.as_ref().unwrap().switch_block;
    let epoch = spec.xdpos.epoch;
    let cps: Vec<Cp> = lines("checkpoints.jsonl");
    let mut diffs: Vec<String> = vec![];
    let (mut by_era, mut comebacks, mut penalised, mut trimmed) = ([0usize; 3], 0, 0, 0);
    let mut most = 0;
    for c in &cps {
        let n = c.number;
        let mut diff = |what: &str| diffs.push(format!("checkpoint {n}: {what}"));
        let (ts, tr) = (
            spec.effective.tip_signing.is_active(n),
            spec.effective.tip_randomize.is_active(n),
        );
        if (ts, tr) != (c.tip_signing, c.tip_randomize) {
            diff("the chain spec and the pinned geth disagree on TIPSigning or TIPRandomize");
        }
        let mut words = HashMap::new();
        for (slot, v) in &c.candidate_words {
            words.insert((validator_0x88::ADDRESS, *slot), U256::from_be_bytes(v.0));
        }
        for (slot, v) in &c.signer_words {
            words.insert((signer_0x89::ADDRESS, *slot), U256::from_be_bytes(v.0));
        }
        let state = Words(words);
        let candidates: Vec<(Address, U256)> = validator_0x88::candidates(&state)
            .into_iter()
            .map(|a| (a, validator_0x88::candidate_cap(&state, a)))
            .collect();
        most = most.max(candidates.len());

        // 1. The candidates the gap block keeps are the archive's snapshot at the gap block.
        let max = max_masternodes(
            c.gap_number,
            Some(APOTHEM_TIP_INCREASE),
            Some(switch),
            APOTHEM_MAX_MASTERNODES_V2,
        );
        let kept = select_candidates(&candidates, max).unwrap();
        trimmed += usize::from(candidates.len() > kept.len());
        if kept.iter().collect::<BTreeSet<_>>() != c.snapshot_gap.iter().collect::<BTreeSet<_>>() {
            diff(&format!(
                "kept {} of {} candidates (max {max}), the snapshot at {} has {}",
                kept.len(),
                candidates.len(),
                c.gap_number,
                c.snapshot_gap.len()
            ));
        }

        // 2. The penalties: the rule's list leads the header's, the rest are comebacks.
        let era = penalty_era(ts, tr);
        by_era[era as usize] += 1;
        let mut counts: BTreeMap<Address, u64> = BTreeMap::new();
        match era {
            PenaltyEra::BlockSigners => {
                for b in &c.signer_blocks {
                    for s in signer_0x89::signers(&state, b.hash) {
                        *counts.entry(s).or_default() += 1;
                    }
                }
            }
            PenaltyEra::Creators => counts.extend(c.creators.iter().copied()),
            PenaltyEra::ComebackOnly => {}
        }
        let base = penalties_v1(era, &c.prev_masternodes, &counts).unwrap();
        penalised += usize::from(!base.is_empty());
        if !c.penalties.starts_with(&base) {
            diff(&format!(
                "penalties_v1 gives {base:?}, the header has {:?}",
                c.penalties
            ));
        } else {
            let rest = &c.penalties[base.len()..];
            // Comebacks (XB-10): penalised LimitPenaltyEpoch + 1 epochs ago and a candidate again,
            // in the order of that header's list. Not derived here, only bounded.
            let pool: Vec<Address> = c
                .comeback
                .iter()
                .flat_map(|p| p.penalties.iter().copied())
                .filter(|a| c.snapshot_gap.contains(a))
                .collect();
            comebacks += usize::from(!rest.is_empty());
            if era == PenaltyEra::BlockSigners && !rest.is_empty() || !is_subsequence(rest, &pool) {
                diff(&format!(
                    "penalties {rest:?} are neither the rule's nor comebacks of {pool:?}"
                ));
            }
        }

        // 3. The list in the header.
        let previous: BTreeMap<u64, Vec<Address>> = c
            .prev_penalties
            .iter()
            .map(|p| (p.number, p.penalties.clone()))
            .collect();
        let removed = checkpoint_penalties(n, epoch, &c.penalties, &previous).unwrap();
        match checkpoint_masternodes(&candidates, max, &removed) {
            Ok(list) => {
                if let Err(e) = verify_checkpoint_list(&c.extra, &list) {
                    diff(&format!(
                        "{} (header {:?}, derived {list:?})",
                        e.code(),
                        c.extra
                    ));
                }
            }
            Err(e) => diff(&e.code()),
        }
    }
    println!(
        "checkpoints: {} (first {}, last {}); eras block-signers {} / creators {} / comeback-only {}; \
         with a penalty from the rule {penalised}; with a comeback {comebacks}; candidates cut by the cap {trimmed}; \
         most candidates {most}; differing {}",
        cps.len(),
        cps.first().map_or(0, |c| c.number),
        cps.last().map_or(0, |c| c.number),
        by_era[0],
        by_era[1],
        by_era[2],
        diffs.len()
    );
    assert!(cps.len() >= 600, "fewer checkpoints than the sample");
    assert!(
        diffs.is_empty(),
        "{} differ:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

// ------------------------------------------------------------------ M1 and M2

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpochRec {
    checkpoint: Option<Bytes>,
    blocks: Vec<BlockRec>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockRec {
    number: u64,
    hash: B256,
    header: Bytes,
    seal_hash: B256,
    creator: Address,
    validator: Option<Address>,
    assigned: Option<Address>,
    assigned_error: Option<String>,
    parent: Bytes,
    masternodes: Vec<Address>,
    parent_masternodes: Vec<Address>,
    signers: Vec<Address>,
}

#[test]
fn m1_and_m2_equal_geth_for_the_sampled_blocks() {
    let spec = Network::Apothem.spec();
    let epoch = spec.xdpos.epoch;
    let recs: Vec<EpochRec> = lines("m1m2.jsonl");
    let (mut total, mut paired, mut unpaired, mut unsigned, mut no_m2, mut out_of_turn) =
        (0, 0, 0, 0, 0, 0);
    for r in &recs {
        let cp = r.checkpoint.as_ref().map(header_of);
        for b in &r.blocks {
            total += 1;
            let h = header_of(&b.header);
            assert_eq!(
                (h.number, h.hash(), h.seal_hash()),
                (b.number, b.hash, b.seal_hash)
            );
            let creator = h.v1_signer().unwrap();
            assert_eq!(creator, b.creator, "creator of {}", b.number);
            let validator = ecrecover(&h.seal_hash(), &h.validator).ok();
            assert_eq!(validator, b.validator, "validator of {}", b.number);
            unsigned += usize::from(validator.is_none());

            // The turn and the creator checks, on the archive's own lists.
            let parent = header_of(&b.parent);
            let parent_creator = (parent.number != 0).then(|| parent.v1_signer().unwrap());
            let want = difficulty(
                &b.parent_masternodes,
                parent.number,
                parent_creator,
                creator,
            )
            .unwrap();
            check_difficulty(h.difficulty.as_limbs()[0] as i64, want).unwrap();
            authorize(
                creator,
                &b.signers,
                &b.masternodes,
                parent_creator,
                h.number,
                epoch,
            )
            .unwrap();
            out_of_turn += usize::from(want != i64::try_from(b.parent_masternodes.len()).unwrap());

            let assigned = assigned_validator(
                &h,
                creator,
                cp.as_ref(),
                epoch,
                spec.effective.tip_randomize.is_active(h.number),
            );
            match (&assigned, &b.assigned, &b.assigned_error) {
                (Ok(a), Some(want), None) => assert_eq!(a, want, "M2 of {}", b.number),
                (Err(_), None, Some(_)) => {}
                other => panic!("block {}: {other:?}", b.number),
            }
            if let (Ok(a), Some(v)) = (&assigned, validator) {
                if h.number > epoch {
                    // Apothem does not enforce the pair (geth `v1DoubleValidation`), so this is a
                    // count and not an assertion; the rule itself is in the negative vectors.
                    let r = check_double_validation(&h, creator, || Ok(*a));
                    assert_eq!(r.is_ok(), *a == v);
                    paired += usize::from(*a == v);
                    unpaired += usize::from(*a != v);
                    no_m2 += usize::from(a.is_zero());
                }
            }
        }
    }
    println!(
        "m1m2: {total} blocks, creator and validator and assigned M2 equal to geth; \
         past the first epoch {paired} pairs hold, {unpaired} do not ({no_m2} with no assignment); {unsigned} without a validator; difficulty and creator checks hold, {out_of_turn} blocks out of turn"
    );
    // 76 epochs of 4 blocks (the switch block's epoch has one): the archive allows 5 requests per second.
    assert!(total >= 300);
}

// ------------------------------------------------------------------ the V1 blocks of the corpus

#[derive(Deserialize)]
struct CorpusLine {
    network: String,
    number: u64,
    header: Bytes,
    parent: Bytes,
    context: CorpusContext,
    oracle: CorpusOracle,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusContext {
    consensus: String,
    #[serde(default)]
    masternodes: Vec<Address>,
    #[serde(default)]
    parent_masternodes: Vec<Address>,
    #[serde(default)]
    signers: Vec<Address>,
}
#[derive(Deserialize)]
struct CorpusOracle {
    signer: Option<Address>,
}

#[test]
fn difficulty_and_creator_rules_hold_on_every_v1_block_of_the_corpus() {
    let mut n_blocks = 0;
    let mut out_of_turn = 0;
    for file in ["apothem.jsonl", "net2040.jsonl"] {
        let path = dir().join("../valid").join(file);
        for l in std::fs::read_to_string(path).unwrap().lines() {
            let v: CorpusLine = serde_json::from_str(l).unwrap();
            if v.context.consensus != "v1" || v.number == 0 {
                continue;
            }
            let (h, parent) = (header_of(&v.header), header_of(&v.parent));
            let creator = v.oracle.signer.unwrap();
            let parent_creator = (parent.number != 0).then(|| parent.v1_signer().unwrap());
            let want = difficulty(
                &v.context.parent_masternodes,
                parent.number,
                parent_creator,
                creator,
            )
            .unwrap();
            check_difficulty(h.difficulty.as_limbs()[0] as i64, want)
                .unwrap_or_else(|e| panic!("{} {}: {e}", v.network, v.number));
            authorize(
                creator,
                &v.context.signers,
                &v.context.masternodes,
                parent_creator,
                h.number,
                900,
            )
            .unwrap_or_else(|e| panic!("{} {}: {e}", v.network, v.number));
            n_blocks += 1;
            out_of_turn +=
                usize::from(want != i64::try_from(v.context.parent_masternodes.len()).unwrap());
        }
    }
    println!("corpus: difficulty and creator rules hold on {n_blocks} V1 blocks ({out_of_turn} out of turn)");
    assert!(n_blocks > 100);
}

// ------------------------------------------------------------------ the hand-off

#[test]
fn the_switch_blocks_of_every_network_are_epoch_aligned() {
    for net in [Network::Apothem, Network::Mainnet] {
        check_switch_block(&net.spec().xdpos).unwrap();
    }
}
