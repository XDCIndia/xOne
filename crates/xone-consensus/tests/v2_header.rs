#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The V2 header verifier against the corpus blocks: QC signatures of real headers, the per-round
//! `minePeriod` spacing (xOneGo #622), and `Validators` and `Penalties` computed from the gap snapshot
//! instead of read from the context (no tolerant mode).

#[path = "../../../conformance/negative-gen/src/vector.rs"]
mod vector;

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::Deserialize;
use vector::Vector;
use xone_chainspec::{ForkBlock, V2Config};
use xone_consensus::v2::{
    cert,
    epoch_switch::{self, SwitchQuery},
    gap_snapshot, Error,
};
use xone_consensus::verify::{Context, HeaderError, HeaderVerifier, V2Context};
use xone_primitives::Header;
use xone_state::{Account, StateRead};

fn header_of(b: &Bytes) -> Header {
    alloy_rlp::decode_exact(b).unwrap()
}

fn v2_vectors(classes: &[&str]) -> Vec<Vector> {
    let mut all = vec![];
    for net in ["apothem", "net2040"] {
        all.extend(vector::load_file(
            &vector::vectors_dir().join(format!("valid/{net}.jsonl")),
        ));
    }
    all.into_iter()
        .filter(|v| classes.contains(&v.class.as_str()))
        .collect()
}

fn ctx_of(v: &Vector) -> V2Context {
    let c = &v.context;
    V2Context {
        masternodes: c.masternodes.clone(),
        penalties: c.penalties.clone(),
        parent_masternodes: (!c.parent_masternodes.is_empty())
            .then(|| c.parent_masternodes.clone()),
        parent_epoch_switch: c.parent_epoch_switch.unwrap(),
    }
}

// ---------------------------------------------------------------- QC of real headers

/// (masternodes that signed the QC of `h`, its QC, threshold) for a V2 block whose parent is a V2 block.
fn qc_of(v: &Vector) -> Option<(Vec<Address>, xone_primitives::QuorumCert, f64)> {
    let (h, parent) = (header_of(&v.header), header_of(&v.parent));
    let spec = vector::spec_of(v);
    let v2 = spec.xdpos.v2.as_ref()?;
    if h.number <= v2.switch_block + 1 || parent.number <= v2.switch_block {
        return None;
    }
    let qc = h.extra_fields_v2().ok()?.quorum_cert;
    let thr = v2
        .config_at_round(qc.proposed_block_info.round)?
        .cert_threshold;
    Some((v.context.parent_masternodes.clone(), qc, thr))
}

#[test]
fn real_qcs_pass_and_their_mutations_fail_for_the_rule_that_applies() {
    let (mut positives, mut mutated) = (0, 0);
    for v in v2_vectors(&["v2", "v2_epoch", "sign"]) {
        let Some((mn, qc, thr)) = qc_of(&v) else {
            continue;
        };
        if qc.proposed_block_info.round == 0 {
            continue;
        }
        cert::verify_qc(&qc, &mn, thr).unwrap_or_else(|e| panic!("{}: {e}", v.id));
        positives += 1;
        if positives % 5 != 0 {
            continue;
        }
        mutated += 1;
        let need = cert::min_signers(mn.len(), thr).unwrap();
        // one signature short
        let mut short = qc.clone();
        short.signatures.truncate(need - 1);
        assert!(
            matches!(
                cert::verify_qc(&short, &mn, thr),
                Err(Error::BelowThreshold { .. })
            ),
            "{}",
            v.id
        );
        // one signature short, padded with repeats of the signatures it has
        let mut padded = short.clone();
        padded.signatures.extend(short.signatures.clone());
        assert!(
            matches!(
                cert::verify_qc(&padded, &mn, thr),
                Err(Error::BelowThreshold { .. })
            ),
            "{}",
            v.id
        );
        // one signer, many copies (F-2026-19525 in its simplest form)
        let mut one = qc.clone();
        one.signatures = vec![qc.signatures[0].clone(); mn.len() + 3];
        assert!(
            matches!(
                cert::verify_qc(&one, &mn, thr),
                Err(Error::BelowThreshold { .. })
            ),
            "{}",
            v.id
        );
        // the full QC with every signature twice still passes: repeats do not count, and do not hurt
        let mut twice = qc.clone();
        twice.signatures.extend(qc.signatures.clone());
        cert::verify_qc(&twice, &mn, thr).unwrap();
        // a signer outside the epoch's list
        let signer = xone_primitives::ecrecover(&cert::qc_digest(&qc), &qc.signatures[0]).unwrap();
        let without: Vec<Address> = mn.iter().filter(|a| **a != signer).copied().collect();
        assert!(
            matches!(
                cert::verify_qc(&qc, &without, thr),
                Err(Error::SignerNotMasternode(_))
            ),
            "{}",
            v.id
        );
    }
    assert!(positives >= 100, "{positives}");
    println!(
        "QC positives from the header corpus: {positives}; {mutated} of them with 5 mutations each"
    );
}

// ---------------------------------------------------------------- minePeriod

fn spaced_vectors() -> Vec<Vector> {
    v2_vectors(&["v2", "v2_epoch", "sign"])
        .into_iter()
        .filter(|v| {
            let spec = vector::spec_of(v);
            header_of(&v.parent).number > spec.xdpos.v2.as_ref().unwrap().switch_block
        })
        .collect()
}

fn run(v: &Vector, spec: &xone_chainspec::ChainSpec, h: &Header) -> Result<(), HeaderError> {
    HeaderVerifier::new(spec).verify(
        h,
        &header_of(&v.parent),
        v.context.now,
        &Context::V2(ctx_of(v)),
    )
}

#[test]
fn a_v2_block_closer_to_its_parent_than_the_min_period_of_its_round_is_rejected() {
    let vs = spaced_vectors();
    assert!(vs.len() >= 100);
    for v in &vs {
        let spec = vector::spec_of(v);
        let (h, parent) = (header_of(&v.header), header_of(&v.parent));
        let round = h.extra_fields_v2().unwrap().round;
        let period = spec
            .xdpos
            .v2
            .as_ref()
            .unwrap()
            .config_at_round(round)
            .unwrap()
            .mine_period;
        let mut early = h.clone();
        early.timestamp = parent.timestamp + period - 1;
        assert!(
            matches!(
                run(v, &spec, &early),
                Err(HeaderError::InvalidTimestamp { .. })
            ),
            "{}: {:?}",
            v.id,
            run(v, &spec, &early)
        );
        // exactly the period passes this check (the seal then no longer matches: another error)
        early.timestamp = parent.timestamp + period;
        assert!(
            !matches!(
                run(v, &spec, &early),
                Err(HeaderError::InvalidTimestamp { .. })
            ),
            "{}",
            v.id
        );
    }
}

#[test]
fn the_min_period_is_the_one_of_the_round_of_the_block() {
    for v in spaced_vectors().iter().take(40) {
        let mut spec = vector::spec_of(v);
        let h = header_of(&v.header);
        let round = h.extra_fields_v2().unwrap().round;
        let gap = h.timestamp - header_of(&v.parent).timestamp;
        let v2 = spec.xdpos.v2.as_mut().unwrap();
        // a config that starts at this round and asks for more than the block has
        v2.configs.insert(
            round,
            V2Config {
                mine_period: gap + 1,
                cert_threshold: 0.667,
                max_masternodes: 108,
                ..V2Config::default()
            },
        );
        let got = run(v, &spec, &h);
        assert!(
            matches!(got, Err(HeaderError::InvalidTimestamp { period, .. }) if period == gap + 1),
            "{}: {got:?}",
            v.id
        );
        // one that starts the round after does not apply to this block
        let v2 = spec.xdpos.v2.as_mut().unwrap();
        let cfg = v2.configs.remove(&round).unwrap();
        v2.configs.insert(round + 1, cfg);
        assert_eq!(run(v, &spec, &h), Ok(()), "{}", v.id);
    }
}

// ---------------------------------------------------------------- Validators and Penalties computed at a switch

struct Words(std::collections::BTreeMap<B256, U256>);
impl StateRead for Words {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, _: Address, slot: B256) -> U256 {
        self.0.get(&slot).copied().unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct GapOut {
    switches: Vec<Rec>,
    bootstrap: Boot,
}
#[derive(Deserialize)]
struct Rec {
    number: u64,
    gap: u64,
    words: Vec<(B256, B256)>,
}
#[derive(Deserialize)]
struct Boot {
    switch_extra: Bytes,
}

fn gap_file(net: &str) -> GapOut {
    let p = format!(
        "{}/../../conformance/vectors/v2/gap-{net}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The derive function of a vector: pool from the gap block words, penalties of the header (the
/// archive sample has no epoch window for these blocks; the penalty engine has its own vectors).
fn derive_for<'a>(
    spec: &'a xone_chainspec::ChainSpec,
    g: &'a GapOut,
    number: u64,
    header_penalties: Vec<Address>,
    edit: impl Fn(&mut Vec<(B256, B256)>) + 'a,
) -> impl Fn(&SwitchQuery) -> Result<epoch_switch::Expected, Error> + 'a {
    move |q: &SwitchQuery| {
        let v2 = spec.xdpos.v2.as_ref().unwrap();
        let rec = g
            .switches
            .iter()
            .find(|r| r.number == number)
            .ok_or(Error::Missing {
                what: "gap vector",
                number,
            })?;
        let pool = if gap_snapshot::uses_v1_list(number, spec.xdpos.epoch, v2.switch_block)? {
            gap_snapshot::bootstrap(&g.bootstrap.switch_extra, v2.switch_block)?
        } else {
            let mut words = rec.words.clone();
            edit(&mut words);
            let state = Words(
                words
                    .iter()
                    .map(|(k, v)| (*k, U256::from_be_bytes(v.0)))
                    .collect(),
            );
            gap_snapshot::build(&state, rec.gap)?
        };
        epoch_switch::lists(spec, q, &pool, |_| Ok(header_penalties.clone()))
    }
}

fn switch_vectors() -> Vec<Vector> {
    v2_vectors(&["v2_epoch"])
}

#[test]
fn a_switch_header_is_accepted_when_its_lists_are_computed_from_the_gap_snapshot() {
    let (mut n, mut ordered) = (0, 0);
    for net in ["apothem", "net2040"] {
        let g = gap_file(net);
        for v in switch_vectors().iter().filter(|v| v.network == net) {
            let (h, parent) = (header_of(&v.header), header_of(&v.parent));
            let spec = vector::spec_of(v);
            let derive = derive_for(&spec, &g, v.number, v.context.penalties.clone(), |_| {});
            let verifier = HeaderVerifier::new(&spec);
            verifier
                .verify_derived(&h, &parent, v.context.now, &ctx_of(v), &derive)
                .unwrap_or_else(|e| panic!("{}: {e}", v.id));
            n += 1;
            // and with the validator order enforced, which only passes if the derived order is the header's
            let mut ordered_spec = spec.clone();
            ordered_spec.validator_order = ForkBlock::At(0);
            let derive = derive_for(
                &ordered_spec,
                &g,
                v.number,
                v.context.penalties.clone(),
                |_| {},
            );
            HeaderVerifier::new(&ordered_spec)
                .verify_derived(&h, &parent, v.context.now, &ctx_of(v), &derive)
                .unwrap_or_else(|e| panic!("{} ordered: {e}", v.id));
            ordered += 1;
        }
    }
    assert!(n >= 25, "{n}");
    println!("switch headers accepted with derived lists: {n} ({ordered} with the order enforced)");
}

#[test]
fn a_wrong_derived_list_rejects_the_switch_block() {
    let mut checked = 0;
    for net in ["apothem", "net2040"] {
        let g = gap_file(net);
        for v in switch_vectors().iter().filter(|v| v.network == net) {
            let spec = vector::spec_of(v);
            let v2 = spec.xdpos.v2.as_ref().unwrap();
            if gap_snapshot::uses_v1_list(v.number, spec.xdpos.epoch, v2.switch_block).unwrap() {
                continue;
            }
            let (h, parent) = (header_of(&v.header), header_of(&v.parent));
            let verifier = HeaderVerifier::new(&spec);
            // a masternode of the header whose candidate slot is emptied before the gap block
            let victim = h.validators[..20].to_vec();
            let drop_victim = move |w: &mut Vec<(B256, B256)>| {
                for (_, val) in w.iter_mut() {
                    if val.0[12..] == victim[..] && val.0[..12] == [0; 12] {
                        *val = B256::ZERO;
                        return;
                    }
                }
            };
            let derive = derive_for(
                &spec,
                &g,
                v.number,
                v.context.penalties.clone(),
                drop_victim,
            );
            let got = verifier.verify_derived(&h, &parent, v.context.now, &ctx_of(v), &derive);
            assert_eq!(got, Err(HeaderError::ValidatorsNotLegit), "{}", v.id);
            // missing gap state is an error, never an empty or a trusted list
            let missing = |_: &SwitchQuery| {
                Err(Error::Missing {
                    what: "0x88 candidates",
                    number: 0,
                })
            };
            let got = verifier.verify_derived(&h, &parent, v.context.now, &ctx_of(v), &missing);
            assert!(
                matches!(
                    got,
                    Err(HeaderError::SwitchDerivation(Error::Missing { .. }))
                ),
                "{}: {got:?}",
                v.id
            );
            // a penalty list that is not the header's
            let extra_pen = |q: &SwitchQuery| {
                let mut e =
                    derive_for(&spec, &g, v.number, v.context.penalties.clone(), |_| {})(q)?;
                e.penalties.push(Address::repeat_byte(7));
                Ok(e)
            };
            let got = verifier.verify_derived(&h, &parent, v.context.now, &ctx_of(v), &extra_pen);
            assert_eq!(got, Err(HeaderError::PenaltiesNotLegit), "{}", v.id);
            // the order of the validators, once enforced
            let mut ordered = spec.clone();
            ordered.validator_order = ForkBlock::At(0);
            let reversed = |q: &SwitchQuery| {
                let mut e =
                    derive_for(&ordered, &g, v.number, v.context.penalties.clone(), |_| {})(q)?;
                e.validators.reverse();
                Ok(e)
            };
            if h.validators.len() > 20 {
                let got = HeaderVerifier::new(&ordered).verify_derived(
                    &h,
                    &parent,
                    v.context.now,
                    &ctx_of(v),
                    &reversed,
                );
                assert_eq!(got, Err(HeaderError::ValidatorsOrder), "{}", v.id);
            }
            checked += 1;
        }
    }
    assert!(checked >= 20, "{checked}");
}
