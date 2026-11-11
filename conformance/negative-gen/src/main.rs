//! Test-only generator of `conformance/vectors/consensus/negative/`.
//!
//! Reads every valid vector, applies each rule's mutation recipe to up to a few
//! eligible vectors and writes `negative/<rule>.jsonl`. The expected error of a
//! rule is written here, next to its recipe: it names the rule the mutation
//! breaks. It is never computed by running the verifier.
//!
//!     cargo run -p xone-negative-gen
#![forbid(unsafe_code)]

#[allow(dead_code)]
mod vector;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{Decodable, Encodable};
use vector::{Mutation, Vector};
use xone_chainspec::ChainSpec;
use xone_consensus::verify::Receipt;
use xone_primitives::{ExtraFieldsV2, Header, EXTRA_SEAL};

/// How many vectors each rule mutates, per network.
const PER_NETWORK: [(&str, usize); 2] = [("apothem", 6), ("net2040", 4)];
/// The order of secp256k1 (SEC 2, section 2.4.1).
const SECP256K1_N: U256 = U256::from_be_bytes([
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
]);

struct Src {
    v: Vector,
    h: Header,
    parent: Header,
    spec: ChainSpec,
    v2: bool,
}

impl Src {
    fn n(&self) -> u64 {
        self.h.number
    }
    fn epoch_switch_v2(&self) -> bool {
        self.v2 && !self.h.validators.is_empty()
    }
    fn extra(&self) -> Option<ExtraFieldsV2> {
        self.h.extra_fields_v2().ok()
    }
    fn qc_round(&self) -> u64 {
        self.extra()
            .map_or(0, |e| e.quorum_cert.proposed_block_info.round)
    }
    fn checkpoint_v1(&self) -> bool {
        !self.v2 && self.n().is_multiple_of(self.spec.xdpos.epoch)
    }
    fn full_v1(&self) -> bool {
        !self.v2 && self.spec.chain_id != 51
    }
}

/// What a recipe edits: the header, and the vector for its body, receipts and context.
struct Out {
    h: Header,
    v: Vector,
}

struct Rule {
    name: &'static str,
    stage: &'static str,
    expect: &'static str,
    recipe: &'static str,
    eligible: fn(&Src) -> bool,
    apply: fn(&mut Out, &Src, &[Vector]),
    activate: &'static [&'static str],
}

fn flip(b: &mut B256) {
    b.0[31] ^= 1;
}

fn edit_extra(o: &mut Out, f: impl FnOnce(&mut ExtraFieldsV2)) {
    let mut x = o.h.extra_fields_v2().unwrap();
    f(&mut x);
    o.h.extra_data = x.encode_extra().into();
}

fn drop_addr(bytes: &Bytes) -> Bytes {
    bytes[..bytes.len() - Address::len_bytes()].to_vec().into()
}

fn without(list: &[Address], x: Option<Address>) -> Vec<Address> {
    list.iter().copied().filter(|a| Some(*a) != x).collect()
}

/// The first transaction of the body sent to the BlockSigners contract (0x89).
fn signing_tx(v: &Vector) -> Option<Bytes> {
    let signers = Address::with_last_byte(0x89);
    v.body
        .transactions
        .iter()
        .find(|t| xone_consensus::verify::tx_recipient(t) == Ok(Some(signers)))
        .cloned()
}

fn other_body<'a>(s: &Src, all: &'a [Vector]) -> Option<&'a Vector> {
    all.iter().find(|o| {
        o.network == s.v.network
            && o.id != s.v.id
            && !o.body.transactions.is_empty()
            && o.body.transactions != s.v.body.transactions
    })
}

macro_rules! rule {
    ($name:literal, $stage:literal, $expect:literal, $recipe:literal, $elig:expr, $apply:expr) => {
        rule!($name, $stage, $expect, $recipe, $elig, $apply, [])
    };
    ($name:literal, $stage:literal, $expect:literal, $recipe:literal, $elig:expr, $apply:expr, $on:expr) => {
        Rule {
            name: $name,
            stage: $stage,
            expect: $expect,
            recipe: $recipe,
            eligible: $elig,
            apply: $apply,
            activate: &$on,
        }
    };
}

fn rules() -> Vec<Rule> {
    vec![
        // ---- common header fields, V1 and V2
        rule!("unsealed_field_set", "header", "UnsealedFields",
            "set withdrawalsHash on a block before Amsterdam",
            |s| !s.spec.amsterdam.is_active(s.n()),
            |o, _, _| o.h.withdrawals_hash = Some(B256::ZERO)),
        rule!("amsterdam_field_missing", "header", "UnsealedFields",
            "clear blockAccessListHash on an Amsterdam block",
            |s| s.spec.amsterdam.is_active(s.n()),
            |o, _, _| o.h.block_access_list_hash = None),
        rule!("nonce_invalid", "header", "InvalidVote",
            "set the nonce to 0x0000000000000001",
            |_| true,
            |o, _, _| o.h.nonce = xone_primitives::BlockNonce::from_u64(1)),
        rule!("mix_digest_nonzero", "header", "InvalidMixDigest",
            "set the last byte of the mix digest",
            |_| true,
            |o, _, _| o.h.mix_digest.0[31] = 1),
        rule!("uncle_hash_wrong", "header", "InvalidUncleHash",
            "flip a bit of the uncle hash",
            |_| true,
            |o, _, _| flip(&mut o.h.uncle_hash)),
        rule!("number_wrong", "header", "UnknownAncestor",
            "number + 1",
            |s| s.v2 || (s.n() % s.spec.xdpos.epoch != 0 && (s.n() + 1) % s.spec.xdpos.epoch != 0),
            |o, _, _| o.h.number += 1),
        rule!("parent_hash_wrong", "header", "UnknownAncestor",
            "flip a bit of the parent hash",
            |_| true,
            |o, _, _| flip(&mut o.h.parent_hash)),
        rule!("timestamp_too_early", "header", "InvalidTimestamp",
            "timestamp = parent timestamp",
            |s| s.parent.number > s.spec.xdpos.v2.as_ref().unwrap().switch_block || !s.v2,
            |o, s, _| o.h.timestamp = s.parent.timestamp),
        rule!("timestamp_future", "header", "FutureBlock",
            "timestamp = now + 1",
            |_| true,
            |o, s, _| o.h.timestamp = s.v.context.now + 1),
        // ---- V1
        rule!("v1_checkpoint_beneficiary", "header", "CheckpointBeneficiary",
            "set the coinbase of a checkpoint block",
            |s| s.checkpoint_v1(),
            |o, _, _| o.h.coinbase = Address::repeat_byte(1)),
        rule!("v1_extra_signers_in_ordinary_block", "header", "ExtraSigners",
            "insert 20 bytes between vanity and seal of a non-checkpoint block",
            |s| !s.v2 && !s.checkpoint_v1() && s.n() < s.spec.xdpos.v2.as_ref().unwrap().switch_block,
            |o, _, _| {
                let mut e = o.h.extra_data.to_vec();
                e.splice(32..32, [0u8; 20]);
                o.h.extra_data = e.into();
            }),
        rule!("v1_checkpoint_signers_misaligned", "header", "InvalidCheckpointSigners",
            "insert 1 byte into the signer list of a checkpoint block",
            |s| s.checkpoint_v1(),
            |o, _, _| {
                let mut e = o.h.extra_data.to_vec();
                e.insert(32, 0);
                o.h.extra_data = e.into();
            }),
        rule!("v1_extra_without_seal", "header", "MissingSignature",
            "cut extra data to 40 bytes",
            |s| !s.v2,
            |o, _, _| o.h.extra_data = o.h.extra_data[..40].to_vec().into()),
        rule!("v1_extra_without_vanity", "header", "MissingVanity",
            "cut extra data to 10 bytes",
            |s| !s.v2,
            |o, _, _| o.h.extra_data = o.h.extra_data[..10].to_vec().into()),
        rule!("v1_difficulty_wrong", "header", "InvalidDifficulty",
            "difficulty = 1,000,000, above any turn-based value",
            |s| !s.v2,
            |o, _, _| o.h.difficulty = U256::from(1_000_000u64)),
        rule!("v1_checkpoint_signers_dropped", "header", "InvalidCheckpointSigners",
            "remove the last address from the signer list of a checkpoint block",
            |s| s.checkpoint_v1() && s.full_v1() && s.h.extra_data.len() >= 32 + 20 + EXTRA_SEAL,
            |o, _, _| {
                let mut e = o.h.extra_data.to_vec();
                let end = e.len() - EXTRA_SEAL;
                e.drain(end - 20..end);
                o.h.extra_data = e.into();
            }),
        rule!("v1_signer_not_authorised", "header", "Unauthorized",
            "remove the seal's signer from the snapshot signers and the masternodes",
            |s| !s.v2 && s.v.oracle.signer.is_some(),
            |o, s, _| {
                let who = s.v.oracle.signer;
                o.v.context.signers = without(&o.v.context.signers, who);
                o.v.context.masternodes = without(&o.v.context.masternodes, who);
            }),
        rule!("v1_validator_missing", "header", "NoValidatorSignature",
            "clear the validator field of a block past the first epoch",
            |s| s.full_v1() && s.n() > s.spec.xdpos.epoch,
            |o, _, _| o.h.validator = Bytes::new()),
        rule!("v1_validator_unrecoverable", "header", "InvalidValidatorSignature",
            "validator = 65 zero bytes",
            |s| s.full_v1() && s.n() > s.spec.xdpos.epoch,
            |o, _, _| o.h.validator = vec![0u8; 65].into()),
        rule!("v1_validator_is_creator", "header", "FailedDoubleValidation",
            "validator = the creator's own seal (the M2 must be another masternode)",
            |s| s.full_v1() && s.n() > s.spec.xdpos.epoch,
            |o, _, _| {
                let n = o.h.extra_data.len() - EXTRA_SEAL;
                o.h.validator = o.h.extra_data[n..].to_vec().into();
            }),
        // ---- V2 header
        rule!("v2_gas_limit_out_of_bound", "header", "InvalidGasLimit",
            "gas limit = 2 * parent gas limit",
            |s| s.v2,
            |o, s, _| o.h.gas_limit = s.parent.gas_limit * 2),
        rule!("v2_gas_used_over_limit", "header", "GasUsedExceedsLimit",
            "gas used = gas limit + 1",
            |s| s.v2,
            |o, _, _| o.h.gas_used = o.h.gas_limit + 1),
        rule!("v2_validator_length", "header", "ValidatorSignatureLength",
            "validator = 64 bytes",
            |s| s.v2,
            |o, _, _| o.h.validator = o.h.validator[..64].to_vec().into()),
        rule!("v2_validator_missing", "header", "NoValidatorSignatureV2",
            "clear the validator field",
            |s| s.v2,
            |o, _, _| o.h.validator = Bytes::new()),
        rule!("v2_extra_version", "header", "InvalidV2Extra",
            "extra data version byte 2 -> 1",
            |s| s.v2,
            |o, _, _| {
                let mut e = o.h.extra_data.to_vec();
                e[0] = 1;
                o.h.extra_data = e.into();
            }),
        rule!("v2_extra_trailing_bytes", "header", "InvalidV2Extra",
            "append one byte to the V2 extra data",
            |s| s.v2,
            |o, _, _| {
                let mut e = o.h.extra_data.to_vec();
                e.push(0);
                o.h.extra_data = e.into();
            }),
        rule!("v2_round_not_above_qc", "header", "RoundInvalid",
            "round = the QC's round",
            |s| s.v2,
            |o, _, _| edit_extra(o, |x| x.round = x.quorum_cert.proposed_block_info.round)),
        rule!("v2_qc_block_hash", "header", "QcEpochInfo",
            "flip a bit of the QC's block hash",
            |s| s.v2,
            |o, _, _| edit_extra(o, |x| flip(&mut x.quorum_cert.proposed_block_info.hash))),
        rule!("v2_qc_signature_bad", "header", "QcSignerNotMasternode",
            "flip the lowest bit of s in the first QC signature",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| {
                let mut sig = x.quorum_cert.signatures[0].to_vec();
                sig[63] ^= 1;
                x.quorum_cert.signatures[0] = sig.into();
            })),
        rule!("v2_qc_signature_high_s", "header", "QcSignatureHighS",
            "replace the first QC signature by (r, N - s, v ^ 1): same signer, malleated",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| {
                let mut sig = x.quorum_cert.signatures[0].to_vec();
                let s = U256::from_be_slice(&sig[32..64]);
                sig[32..64].copy_from_slice(&(SECP256K1_N - s).to_be_bytes::<32>());
                sig[64] ^= 1;
                x.quorum_cert.signatures[0] = sig.into();
            })),
        rule!("v2_qc_signature_unrecoverable", "header", "QcSignatureUnrecoverable",
            "recovery id of the first QC signature = 4",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| {
                let mut sig = x.quorum_cert.signatures[0].to_vec();
                sig[64] = 4;
                x.quorum_cert.signatures[0] = sig.into();
            })),
        rule!("v2_qc_below_threshold", "header", "QcBelowThreshold",
            "keep only the first QC signature",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| x.quorum_cert.signatures.truncate(1))),
        rule!("v2_qc_duplicate_signer", "header", "QcBelowThreshold",
            "replace every QC signature by a copy of the first (same length, one signer)",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| {
                let first = x.quorum_cert.signatures[0].clone();
                x.quorum_cert.signatures.iter_mut().for_each(|s| *s = first.clone());
            })),
        rule!("v2_qc_gap_number_in_header", "header", "QcSignerNotMasternode",
            "QC gap number + 1 (the signatures cover it, so they stop recovering to masternodes)",
            |s| s.v2 && s.qc_round() > 0,
            |o, _, _| edit_extra(o, |x| x.quorum_cert.gap_number += 1)),
        rule!("v2_qc_gap_number_epoch_switch", "header", "QcGapNumber",
            "context: the parent's epoch-switch block one epoch later, so the gap the QC must carry differs",
            |s| s.v2 && s.v.context.parent_epoch_switch.is_some(),
            |o, s, _| o.v.context.parent_epoch_switch = Some(s.v.context.parent_epoch_switch.unwrap() + s.spec.xdpos.epoch)),
        rule!("v2_base_fee_wrong_tier", "header", "InvalidBaseFee",
            "swap the base fee for the other gas price tier",
            |s| s.v2 && s.h.base_fee.is_some() && s.h.withdrawals_hash.is_none(),
            |o, _, _| {
                let low = U256::from(12_500_000_000u64);
                o.h.base_fee = Some(if o.h.base_fee == Some(low) { low * U256::from(50) } else { low });
            }),
        rule!("v2_base_fee_missing", "header", "MissingBaseFee",
            "clear the base fee after EIP-1559",
            |s| s.v2 && s.h.base_fee.is_some() && s.h.withdrawals_hash.is_none(),
            |o, _, _| o.h.base_fee = None),
        rule!("v2_difficulty_wrong", "header", "InvalidDifficulty",
            "difficulty = 2",
            |s| s.v2,
            |o, _, _| o.h.difficulty = U256::from(2)),
        rule!("v2_validators_dropped", "header", "ValidatorsNotLegit",
            "remove the last address from the epoch-switch validators",
            |s| s.epoch_switch_v2() && s.h.validators.len() >= 40,
            |o, _, _| o.h.validators = drop_addr(&o.h.validators)),
        rule!("v2_validators_reordered", "header", "ValidatorsOrder",
            "swap the first two epoch-switch validators, with ValidatorOrderBlock switched on (it is unscheduled on Apothem)",
            |s| s.epoch_switch_v2() && s.h.validators.len() >= 40,
            |o, _, _| {
                let mut v = o.h.validators.to_vec();
                let (a, b) = v.split_at_mut(20);
                a.swap_with_slice(&mut b[..20]);
                o.h.validators = v.into();
            },
            ["validator_order"]),
        rule!("v2_validators_empty_at_switch", "header", "EmptyEpochSwitchValidators",
            "clear the validators of an epoch switch",
            |s| s.epoch_switch_v2(),
            |o, _, _| o.h.validators = Bytes::new()),
        rule!("v2_validators_misaligned", "header", "InvalidCheckpointSigners",
            "append one byte to the epoch-switch validators",
            |s| s.epoch_switch_v2(),
            |o, _, _| o.h.validators = [o.h.validators.as_ref(), &[0]].concat().into()),
        rule!("v2_penalties_wrong", "header", "PenaltiesNotLegit",
            "drop the last penalty, or add one if there is none",
            |s| s.epoch_switch_v2(),
            |o, _, _| {
                o.h.penalties = if o.h.penalties.is_empty() {
                    vec![1u8; 20].into()
                } else {
                    drop_addr(&o.h.penalties)
                }
            }),
        rule!("v2_checkpoint_nonce", "header", "InvalidCheckpointVote",
            "nonce = 0xffffffffffffffff on an epoch switch",
            |s| s.epoch_switch_v2(),
            |o, _, _| o.h.nonce = xone_primitives::BlockNonce(alloy_primitives::B64::repeat_byte(0xff))),
        rule!("v2_validators_in_ordinary_block", "header", "InvalidFieldInNonEpochSwitch",
            "set 20 bytes of validators on a block that is not an epoch switch",
            |s| s.v2 && !s.epoch_switch_v2(),
            |o, _, _| o.h.validators = vec![1u8; 20].into()),
        rule!("v2_penalties_in_ordinary_block", "header", "InvalidFieldInNonEpochSwitch",
            "set 20 bytes of penalties on a block that is not an epoch switch",
            |s| s.v2 && !s.epoch_switch_v2(),
            |o, _, _| o.h.penalties = vec![1u8; 20].into()),
        rule!("v2_validator_not_a_masternode", "header", "ValidatorNotWithinMasternodes",
            "context: remove the block's signer from the masternodes",
            |s| s.v2 && !s.epoch_switch_v2() && s.v.oracle.signer.is_some(),
            |o, s, _| o.v.context.masternodes = without(&o.v.context.masternodes, s.v.oracle.signer)),
        rule!("v2_not_its_turn", "header", "NotItsTurn",
            "context: rotate the masternodes by one, so another one is the leader",
            |s| s.v2 && !s.epoch_switch_v2() && s.v.context.masternodes.len() > 1,
            |o, _, _| o.v.context.masternodes.rotate_left(1)),
        // ---- body and roots
        rule!("body_tx_root_bad", "body", "TxRootMismatch",
            "flip a bit of the header's transaction root",
            |_| true,
            |o, _, _| flip(&mut o.h.tx_root)),
        rule!("body_receipt_root_bad", "body", "ReceiptRootMismatch",
            "flip a bit of the header's receipts root",
            |_| true,
            |o, _, _| flip(&mut o.h.receipt_root)),
        rule!("body_bloom_bad", "body", "BloomMismatch",
            "flip a bit of the header's logs bloom",
            |_| true,
            |o, _, _| o.h.bloom.0[0] ^= 1),
        rule!("body_uncle_hash_wrong", "body", "UncleHashMismatch",
            "flip a bit of the header's uncle hash",
            |_| true,
            |o, _, _| flip(&mut o.h.uncle_hash)),
        rule!("body_uncle_added", "body", "UnclesNotAllowed",
            "add the parent header as an uncle",
            |_| true,
            |o, s, _| o.v.body.uncles.push(alloy_rlp::encode(&s.parent).into())),
        rule!("body_txs_reordered", "body", "TxRootMismatch",
            "swap the first two transactions (and their receipts)",
            |s| s.v.body.transactions.len() >= 2 && s.v.body.transactions[0] != s.v.body.transactions[1],
            |o, _, _| {
                o.v.body.transactions.swap(0, 1);
                o.v.receipts.swap(0, 1);
            }),
        rule!("body_sign_tx_dropped", "body", "TxRootMismatch",
            "drop the first transaction (a signing transaction) of a signing block and its receipt",
            |s| s.v.class == "sign" && !s.v.body.transactions.is_empty(),
            |o, _, _| {
                o.v.body.transactions.remove(0);
                o.v.receipts.remove(0);
            }),
        rule!("body_tx_dropped", "body", "TxRootMismatch",
            "drop the last transaction, keep the receipts",
            |s| !s.v.body.transactions.is_empty(),
            |o, _, _| {
                o.v.body.transactions.pop();
            }),
        rule!("body_tx_appended", "body", "TxRootMismatch",
            "append a copy of the first transaction",
            |s| !s.v.body.transactions.is_empty(),
            |o, _, _| {
                let first = o.v.body.transactions[0].clone();
                o.v.body.transactions.push(first);
            }),
        rule!("body_from_another_block", "body", "TxRootMismatch",
            "take the transactions and receipts of another block of the same network",
            |s| s.v.body.transactions.is_empty(),
            |o, s, all| {
                let other = other_body(s, all).expect("a block with transactions");
                o.v.body.transactions = other.body.transactions.clone();
                o.v.receipts = other.receipts.clone();
            }),
        rule!("body_receipt_dropped", "body", "ReceiptCount",
            "drop the last receipt",
            |s| !s.v.receipts.is_empty(),
            |o, _, _| {
                o.v.receipts.pop();
            }),
        rule!("body_receipt_gas_changed", "body", "ReceiptRootMismatch",
            "cumulative gas used of the first receipt + 1",
            |s| !s.v.receipts.is_empty(),
            |o, _, _| {
                let mut r = Receipt::decode(&o.v.receipts[0]).unwrap();
                r.cumulative_gas_used += 1;
                o.v.receipts[0] = r.encode_consensus().into();
            }),
        rule!("body_too_many_signing_txs", "body", "TooManySigningTxs",
            "append 2 * maxMasternodesV2 + 1 copies of a signing transaction, with SigningTxBoundBlock switched on (it is unscheduled on Apothem)",
            |s| s.v.class == "sign" && s.spec.effective.tip_signing.is_active(s.n()) && signing_tx(&s.v).is_some(),
            |o, s, _| {
                let sign = signing_tx(&s.v).unwrap();
                let more = 2 * s.spec.xdpos.max_masternodes_v2 as usize + 1;
                o.v.body.transactions.extend(std::iter::repeat_n(sign, more));
            },
            ["signing_tx_bound"]),
        rule!("body_withdrawals_missing", "body", "WithdrawalsMissing",
            "drop the withdrawals list of a block whose header commits to one",
            |s| s.h.withdrawals_hash.is_some(),
            |o, _, _| o.v.body.withdrawals = None),
        rule!("body_withdrawal_added", "body", "WithdrawalsRootMismatch",
            "add one withdrawal to the empty list",
            |s| s.h.withdrawals_hash.is_some(),
            |o, _, _| o.v.body.withdrawals = Some(vec![Bytes::from_static(&[0xc0])])),
        rule!("body_access_list_hash_missing", "body", "AccessListHashMissing",
            "clear blockAccessListHash on an Amsterdam block",
            |s| s.spec.amsterdam.is_active(s.n()),
            |o, _, _| o.h.block_access_list_hash = None),
    ]
}

fn main() {
    let dir = vector::vectors_dir();
    let mut all = vec![];
    for net in ["apothem", "net2040"] {
        all.extend(vector::load_file(&dir.join(format!("valid/{net}.jsonl"))));
    }
    let srcs: Vec<Src> = all
        .iter()
        .map(|v| {
            let spec = vector::spec(&v.network);
            let v2 = v.context.consensus == "v2";
            Src {
                h: Header::decode(&mut v.header.as_ref()).unwrap(),
                parent: Header::decode(&mut v.parent.as_ref()).unwrap(),
                v: v.clone(),
                spec,
                v2,
            }
        })
        .collect();

    std::fs::create_dir_all(dir.join("negative")).unwrap();
    let mut counts = vec![];
    for rule in rules() {
        let mut lines = String::new();
        let mut made = 0;
        for (network, want) in PER_NETWORK {
            let eligible: Vec<&Src> = srcs
                .iter()
                .filter(|s| s.v.network == network && (rule.eligible)(s))
                .collect();
            let step = (eligible.len() / want).max(1);
            for s in eligible.iter().step_by(step).take(want) {
                let mut out = Out {
                    h: s.h.clone(),
                    v: s.v.clone(),
                };
                (rule.apply)(&mut out, s, &all);
                let mut header = Vec::new();
                out.h.encode(&mut header);
                out.v.header = header.into();
                out.v.hash = out.h.hash();
                out.v.id = format!("{}.{}", s.v.id, rule.name);
                out.v.mutation = Some(Mutation {
                    rule: rule.name.into(),
                    stage: rule.stage.into(),
                    recipe: rule.recipe.into(),
                    expect: rule.expect.into(),
                    source: s.v.id.clone(),
                    activate: rule.activate.iter().map(|a| a.to_string()).collect(),
                });
                lines += &serde_json::to_string(&out.v).unwrap();
                lines.push('\n');
                made += 1;
            }
        }
        assert!(made > 0, "rule {} has no eligible vector", rule.name);
        std::fs::write(dir.join(format!("negative/{}.jsonl", rule.name)), lines).unwrap();
        counts.push((rule.name, made));
    }
    for (name, n) in &counts {
        println!("{n:3} {name}");
    }
    println!(
        "{} negative vectors, {} rules",
        counts.iter().map(|c| c.1).sum::<usize>(),
        counts.len()
    );
}
