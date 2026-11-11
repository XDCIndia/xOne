//! Test-only generator of `conformance/vectors/consensus/v1/negative.jsonl`: pairs of an input as
//! the archive has it and the same input with one change, for the V1 rules of
//! `xone_consensus::v1`. Sources are the vectors the V1 oracle wrote (`m1m2.jsonl`,
//! `checkpoints.jsonl`). The expected error of a rule is written here, next to its recipe; it is
//! never computed by running the verifier.
//!
//!     cargo run -p xone-negative-gen --bin xone-negative-gen-v1
#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../v1case.rs"]
mod v1case;

use alloy_primitives::{Address, Bytes};
use serde::Deserialize;
use v1case::{Case, Input};
use xone_consensus::v1::turn::difficulty;
use xone_primitives::{Header, EXTRA_SEAL};

/// Cases per rule.
const PER_RULE: usize = 6;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpochRec {
    blocks: Vec<BlockRec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockRec {
    number: u64,
    header: Bytes,
    parent: Bytes,
    creator: Address,
    validator: Option<Address>,
    assigned: Option<Address>,
    masternodes: Vec<Address>,
    parent_masternodes: Vec<Address>,
    signers: Vec<Address>,
}

#[derive(Deserialize)]
struct CpRec {
    number: u64,
    extra: Vec<Address>,
    penalties: Vec<Address>,
}

fn load<T: for<'de> Deserialize<'de>>(name: &str) -> Vec<T> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vectors/consensus/v1")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn header_of(b: &Bytes) -> Header {
    alloy_rlp::decode_exact(b).unwrap()
}

fn encode(h: &Header) -> Bytes {
    alloy_rlp::encode(h).into()
}

/// Up to `PER_RULE` items, evenly spread over the eligible ones.
fn spread<T>(items: Vec<T>) -> Vec<T> {
    let step = (items.len() / PER_RULE).max(1);
    items.into_iter().step_by(step).take(PER_RULE).collect()
}

struct Out(Vec<Case>);

impl Out {
    fn rule(&mut self, rule: &str, recipe: &str, expect: &str, pairs: Vec<(String, Input, Input)>) {
        assert!(!pairs.is_empty(), "rule {rule} has no eligible source");
        for (id, accept, reject) in spread(pairs) {
            self.0.push(Case {
                id: format!("{id}.{rule}"),
                rule: rule.into(),
                recipe: recipe.into(),
                expect: expect.into(),
                accept,
                reject,
            });
        }
    }
}

fn main() {
    let epochs: Vec<EpochRec> = load("m1m2.jsonl");
    let checkpoints: Vec<CpRec> = load("checkpoints.jsonl");
    let mut out = Out(vec![]);

    // Blocks past the first epoch, with the context the rules read.
    struct Blk<'a> {
        b: &'a BlockRec,
        h: Header,
        parent_creator: Option<Address>,
        want: i64,
    }
    let blocks: Vec<Blk> = epochs
        .iter()
        .flat_map(|e| &e.blocks)
        .filter(|b| b.number > 900)
        .map(|b| {
            let (h, parent) = (header_of(&b.header), header_of(&b.parent));
            let parent_creator = (parent.number != 0).then(|| parent.v1_signer().unwrap());
            let want = difficulty(
                &b.parent_masternodes,
                parent.number,
                parent_creator,
                b.creator,
            )
            .unwrap();
            Blk {
                b,
                h,
                parent_creator,
                want,
            }
        })
        .collect();
    let id = |b: &Blk| format!("apothem-{}", b.b.number);
    let dv = |b: &Blk, h: &Header, assigned: Address| Input::DoubleValidation {
        header: encode(h),
        creator: b.b.creator,
        assigned,
    };
    let paired = |b: &Blk| b.b.validator.is_some() && b.b.validator == b.b.assigned;

    out.rule(
        "v1_wrong_m2",
        "validator = the creator's own seal; the M2 assigned to the creator is another masternode",
        "FailedDoubleValidation",
        blocks
            .iter()
            .filter(|b| paired(b) && b.b.assigned != Some(b.b.creator))
            .map(|b| {
                let mut h = b.h.clone();
                h.validator = h.extra_data[h.extra_data.len() - EXTRA_SEAL..]
                    .to_vec()
                    .into();
                (
                    id(b),
                    dv(b, &b.h, b.b.assigned.unwrap()),
                    dv(b, &h, b.b.assigned.unwrap()),
                )
            })
            .collect(),
    );
    out.rule(
        "v1_validator_missing",
        "clear the validator field of a block past the first epoch",
        "NoValidatorSignature",
        blocks
            .iter()
            .filter(|b| paired(b))
            .map(|b| {
                let mut h = b.h.clone();
                h.validator = Bytes::new();
                (
                    id(b),
                    dv(b, &b.h, b.b.assigned.unwrap()),
                    dv(b, &h, b.b.assigned.unwrap()),
                )
            })
            .collect(),
    );
    out.rule(
        "v1_validator_unrecoverable",
        "validator = 65 zero bytes",
        "InvalidValidatorSignature",
        blocks
            .iter()
            .filter(|b| paired(b))
            .map(|b| {
                let mut h = b.h.clone();
                h.validator = vec![0u8; 65].into();
                (
                    id(b),
                    dv(b, &b.h, b.b.assigned.unwrap()),
                    dv(b, &h, b.b.assigned.unwrap()),
                )
            })
            .collect(),
    );

    let auth =
        |b: &Blk, signers: Vec<Address>, masternodes: Vec<Address>, recent: Option<Address>| {
            Input::Authorize {
                creator: b.b.creator,
                signers,
                masternodes,
                recent_creator: recent,
                number: b.b.number,
                epoch: 900,
            }
        };
    let accepted = |b: &Blk| {
        auth(
            b,
            b.b.signers.clone(),
            b.b.masternodes.clone(),
            b.parent_creator,
        )
    };
    out.rule(
        "v1_creator_not_authorised",
        "remove the creator from the snapshot signers and from the masternodes",
        "Unauthorized",
        blocks
            .iter()
            .map(|b| {
                let without =
                    |l: &[Address]| l.iter().copied().filter(|a| *a != b.b.creator).collect();
                (
                    id(b),
                    accepted(b),
                    auth(
                        b,
                        without(&b.b.signers),
                        without(&b.b.masternodes),
                        b.parent_creator,
                    ),
                )
            })
            .collect(),
    );
    out.rule(
        "v1_creator_signed_the_parent",
        "the newest recent signer is the creator itself",
        "Unauthorized",
        blocks
            .iter()
            .filter(|b| b.b.masternodes.len() > 1 && b.b.number % 900 != 0)
            .map(|b| {
                (
                    id(b),
                    accepted(b),
                    auth(
                        b,
                        b.b.signers.clone(),
                        b.b.masternodes.clone(),
                        Some(b.b.creator),
                    ),
                )
            })
            .collect(),
    );

    let len = |b: &Blk| i64::try_from(b.b.parent_masternodes.len()).unwrap();
    let diff = |have: i64, b: &Blk| Input::Difficulty { have, want: b.want };
    let archive = |b: &Blk| b.h.difficulty.as_limbs()[0] as i64;
    out.rule(
        "v1_difficulty_out_of_turn_claims_in_turn",
        "difficulty = the number of masternodes, which only the in-turn creator may carry",
        "InvalidDifficulty",
        blocks
            .iter()
            .filter(|b| b.want < len(b) && archive(b) == b.want)
            .map(|b| (id(b), diff(archive(b), b), diff(len(b), b)))
            .collect(),
    );
    out.rule(
        "v1_difficulty_in_turn_claims_out_of_turn",
        "difficulty = the number of masternodes - 1 on an in-turn block",
        "InvalidDifficulty",
        blocks
            .iter()
            .filter(|b| b.want == len(b) && archive(b) == b.want)
            .map(|b| (id(b), diff(archive(b), b), diff(len(b) - 1, b)))
            .collect(),
    );

    let cps: Vec<&CpRec> = checkpoints.iter().filter(|c| c.extra.len() >= 3).collect();
    let list = |c: &CpRec, listed: Vec<Address>| Input::CheckpointList {
        listed,
        expected: c.extra.clone(),
    };
    let cp_id = |c: &CpRec| format!("apothem-{}", c.number);
    out.rule(
        "v1_checkpoint_list_dropped",
        "remove the last address from the masternode list of a checkpoint",
        "InvalidCheckpointSigners",
        cps.iter()
            .map(|c| {
                (
                    cp_id(c),
                    list(c, c.extra.clone()),
                    list(c, c.extra[..c.extra.len() - 1].to_vec()),
                )
            })
            .collect(),
    );
    out.rule(
        "v1_checkpoint_list_extra",
        "append an address that is not a masternode",
        "InvalidCheckpointSigners",
        cps.iter()
            .map(|c| {
                let mut more = c.extra.clone();
                more.push(Address::repeat_byte(0xaa));
                (cp_id(c), list(c, c.extra.clone()), list(c, more))
            })
            .collect(),
    );
    out.rule(
        "v1_checkpoint_list_reordered",
        "swap the first two addresses: the same set, another order",
        "CheckpointOrder",
        cps.iter()
            .map(|c| {
                let mut swapped = c.extra.clone();
                swapped.swap(0, 1);
                (cp_id(c), list(c, c.extra.clone()), list(c, swapped))
            })
            .collect(),
    );
    let pens = |c: &CpRec, computed: Vec<Address>| Input::CheckpointPenalties {
        header: c
            .penalties
            .iter()
            .flat_map(|a| a.0)
            .collect::<Vec<u8>>()
            .into(),
        computed,
    };
    out.rule(
        "v1_checkpoint_penalties_wrong",
        "the computed penalty list has one more address than the header's",
        "InvalidCheckpointPenalties",
        checkpoints
            .iter()
            .filter(|c| !c.penalties.is_empty())
            .map(|c| {
                let mut more = c.penalties.clone();
                more.push(Address::repeat_byte(0xbb));
                (cp_id(c), pens(c, c.penalties.clone()), pens(c, more))
            })
            .collect(),
    );

    let switch = 56_828_700;
    out.0.push(Case {
        id: "apothem.v1_switch_block_not_epoch_aligned".into(),
        rule: "v1_switch_block_not_epoch_aligned".into(),
        recipe: "V2 switch block + 1 (not a multiple of the epoch 900)".into(),
        expect: "SwitchBlockNotEpochAligned".into(),
        accept: Input::SwitchBlock {
            switch_block: switch,
        },
        reject: Input::SwitchBlock {
            switch_block: switch + 1,
        },
    });

    let mut text = String::new();
    for c in &out.0 {
        text += &serde_json::to_string(c).unwrap();
        text.push('\n');
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vectors/consensus/v1/negative.jsonl");
    std::fs::write(&path, text).unwrap();
    let mut rules: Vec<(&str, usize)> = vec![];
    for c in &out.0 {
        match rules.last_mut() {
            Some((r, n)) if *r == c.rule => *n += 1,
            _ => rules.push((&c.rule, 1)),
        }
    }
    for (r, n) in &rules {
        println!("{n:3} {r}");
    }
    println!("{} negative cases, {} rules", out.0.len(), rules.len());
}
