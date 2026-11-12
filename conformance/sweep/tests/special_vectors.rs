//! XB-08 against the Apothem archive: sign-transaction receipts and the receipts root, the
//! `TIPSigning` block, fee recipients, the fixed base fee and its negative vectors. Every expected
//! value is read from `conformance/vectors/special/` (written by `xone-sweep special`) or from
//! `conformance/vectors/special/base_fee_negative.jsonl`. A `mutant_*` test runs the same check under one wrong
//! rule and asserts that the check then fails.

use alloy_primitives::{Address, Bloom, B256, U256};
use alloy_rlp::Decodable;
use serde_json::Value;
use std::{collections::BTreeMap, str::FromStr};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_evm::{
    basefee::{self, BaseFeeError},
    fees,
    route::{route, Route},
    spec::{rules, XdcRules},
    special::{apply_empty_tx, apply_sign_tx, tip_signing_wipe, Receipt},
};
use xone_primitives::{Header, WideBalance};
use xone_state::{trie, Account, FlatWrite, MemFlat, StateDiff};

fn vec_path(p: &str) -> String {
    format!(
        "{}/../../conformance/vectors/{p}",
        env!("CARGO_MANIFEST_DIR")
    )
}
fn lines(p: &str) -> Vec<Value> {
    std::fs::read_to_string(vec_path(p))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn a(v: &Value) -> Address {
    Address::from_str(
        v.as_str()
            .unwrap_or("0x0000000000000000000000000000000000000000"),
    )
    .unwrap()
}
fn q(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).ok())
        .unwrap()
}
fn spec_of(v: &Value) -> ChainSpec {
    Network::from_str(v["network"].as_str().unwrap())
        .unwrap()
        .spec()
}

type Tweak<'a> = &'a dyn Fn(&mut XdcRules);
type Verify<'a> = &'a dyn Fn(&ChainSpec, u64, Option<U256>) -> Result<(), BaseFeeError>;

/// Sign and empty transactions of whole blocks: route, receipt fields, the receipts root of the
/// block and the nonce of sign-only senders. Returns `(blocks, sign txs)`.
fn check_sign_blocks(tweak: Tweak) -> Result<(usize, usize), String> {
    let (mut blocks, mut signs) = (0, 0);
    for v in lines("special/sign_blocks.jsonl") {
        let (n, spec) = (v["number"].as_u64().unwrap(), spec_of(&v));
        let mut r = rules(&spec, n).unwrap();
        tweak(&mut r);
        let mut prev = 0;
        let mut enc = vec![];
        let mut signed: Vec<(Address, u64)> = vec![];
        for t in v["txs"].as_array().unwrap() {
            let (from, to, nonce, arch) =
                (a(&t["from"]), a(&t["to"]), q(&t["nonce"]), &t["receipt"]);
            let at = |what: &str, mine: String, theirs: String| {
                Err(format!(
                    "block {n} tx from {from}: {what}: xone {mine}, archive {theirs}"
                ))
            };
            let mine = match route(&r, Some(to)) {
                Route::SignTx => {
                    signs += 1;
                    signed.push((from, nonce));
                    apply_sign_tx(&spec, n, nonce, nonce, prev)
                        .map_err(|e| e.to_string())?
                        .1
                }
                Route::XdcxEmpty => {
                    apply_empty_tx(&spec, n, to, prev).map_err(|e| e.to_string())?
                }
                Route::Evm | Route::RandomizeTx => {
                    // An EVM transaction always pays at least its intrinsic gas.
                    if q(&arch["gasUsed"]) == 0 {
                        return at("routed to the EVM", "gas charged".into(), "gas 0".into());
                    }
                    let logs = arch["logs"].as_array().unwrap().iter().map(|l| {
                        alloy_primitives::Log::new_unchecked(
                            a(&l["address"]),
                            l["topics"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|x| B256::from_str(x.as_str().unwrap()).unwrap())
                                .collect(),
                            alloy_primitives::Bytes::from_str(l["data"].as_str().unwrap()).unwrap(),
                        )
                    });
                    Receipt::new(
                        q(&t["ty"]) as u8,
                        q(&arch["status"]) == 1,
                        q(&arch["cumulativeGasUsed"]),
                        q(&arch["gasUsed"]),
                        logs.collect(),
                    )
                }
            };
            if !r.sign_tx_route || route(&r, Some(to)) != Route::SignTx {
                // the archive's own receipt: nothing of ours to compare
            } else {
                if mine.gas_used != q(&arch["gasUsed"]) {
                    return at(
                        "gasUsed",
                        mine.gas_used.to_string(),
                        arch["gasUsed"].to_string(),
                    );
                }
                if u64::from(mine.success) != q(&arch["status"]) {
                    return at(
                        "status",
                        mine.success.to_string(),
                        arch["status"].to_string(),
                    );
                }
                if mine.cumulative_gas_used != q(&arch["cumulativeGasUsed"]) {
                    return at(
                        "cumulativeGasUsed",
                        mine.cumulative_gas_used.to_string(),
                        arch["cumulativeGasUsed"].to_string(),
                    );
                }
                if mine.bloom != Bloom::from_str(arch["logsBloom"].as_str().unwrap()).unwrap() {
                    return at(
                        "logsBloom",
                        format!("{:#x}", mine.bloom),
                        arch["logsBloom"].to_string(),
                    );
                }
                let theirs = arch["logs"].as_array().unwrap();
                if mine.logs.len() != theirs.len()
                    || mine.logs.iter().zip(theirs).any(|(m, t)| {
                        m.address != a(&t["address"])
                            || m.data
                                .topics()
                                .iter()
                                .map(|x| format!("{x:#x}"))
                                .collect::<Vec<_>>()
                                != t["topics"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|x| x.as_str().unwrap().to_string())
                                    .collect::<Vec<_>>()
                            || m.data.data.as_ref()
                                != alloy_primitives::hex::decode(t["data"].as_str().unwrap())
                                    .unwrap()
                    })
                {
                    return at("logs", format!("{:?}", mine.logs), arch["logs"].to_string());
                }
            }
            prev = q(&arch["cumulativeGasUsed"]);
            enc.push(mine.encode());
        }
        let root = alloy_trie::root::ordered_trie_root_encoded(&enc);
        if format!("{root:#x}") != v["receiptsRoot"].as_str().unwrap() {
            return Err(format!(
                "block {n}: receipts root xone {root:#x}, header {}",
                v["receiptsRoot"]
            ));
        }
        for s in v["nonces"].as_array().unwrap() {
            let from = a(&s["from"]);
            let mut nonce = q(&s["before"]);
            for (_, tn) in signed.iter().filter(|(f, _)| *f == from) {
                nonce = apply_sign_tx(&spec, n, nonce, *tn, 0)
                    .map_err(|e| format!("block {n}: {e}"))?
                    .0;
            }
            if nonce != q(&s["after"]) {
                return Err(format!(
                    "block {n}: nonce of {from}: xone {nonce}, archive {}",
                    s["after"]
                ));
            }
        }
        blocks += 1;
    }
    Ok((blocks, signs))
}

#[test]
fn sign_receipts_nonces_and_receipts_root_equal_the_archive() {
    let (blocks, signs) = check_sign_blocks(&|_| {}).unwrap();
    assert!(signs >= 200, "{signs} sign txs");
    eprintln!("{blocks} blocks, {signs} sign txs");
}

/// Named mutant: a sign transaction is charged gas, that is, it goes to the EVM
/// (xOneGo 707546fa and the plan A3 lesson).
#[test]
fn mutant_sign_tx_charged_gas_is_detected() {
    assert!(check_sign_blocks(&|r| r.sign_tx_route = false).is_err());
}

#[test]
fn sign_route_starts_at_tip_signing_and_is_geth_ge() {
    for net in Network::ALL {
        let spec = net.spec();
        let ForkBlock::At(tip) = spec.effective.tip_signing else {
            continue;
        };
        let to = Some(xone_evm::route::BLOCK_SIGNERS);
        if tip > 0 {
            assert_eq!(route(&rules(&spec, tip - 1).unwrap(), to), Route::Evm);
        }
        assert_eq!(route(&rules(&spec, tip).unwrap(), to), Route::SignTx);
        assert_eq!(route(&rules(&spec, tip + 1).unwrap(), to), Route::SignTx);
    }
}

fn dec(v: &Value) -> U256 {
    U256::from_str_radix(v.as_str().unwrap(), 10).unwrap()
}

/// The block `TIPSigning` of Apothem from the full account set of its parent state.
fn check_tip_signing(skip_rule: bool) -> Result<(), String> {
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(vec_path("special/tip_signing_apothem.json")).unwrap(),
    )
    .unwrap();
    let (spec, n) = (spec_of(&v), v["number"].as_u64().unwrap());
    assert_eq!(
        v["transactions"], 0,
        "the block carries no transaction: the whole change is the rule"
    );
    let mut flat = MemFlat::default();
    // Accounts whose address the archive does not know (`incompletes`): only their hashed key.
    let mut anonymous: Vec<(B256, Vec<u8>)> = vec![];
    for x in v["accounts"].as_array().unwrap() {
        // The API saturates a balance wider than 256 bits; the leaf of the account proof is exact.
        let acc = match x["leaf"].as_str() {
            Some(leaf) => {
                let bytes = alloy_primitives::hex::decode(leaf).unwrap();
                let acc = Account::decode(&mut bytes.as_slice()).unwrap();
                assert_eq!(acc.rlp(), bytes, "account leaf round trip");
                acc
            }
            None => Account {
                nonce: x["nonce"].as_u64().unwrap(),
                balance: WideBalance::from(dec(&x["balance"])),
                storage_root: B256::from_str(x["root"].as_str().unwrap()).unwrap(),
                code_hash: B256::from_str(x["codeHash"].as_str().unwrap()).unwrap(),
            },
        };
        if x["address"].is_null() {
            anonymous.push((
                B256::from_str(x["key"].as_str().unwrap()).unwrap(),
                acc.rlp(),
            ));
            continue;
        }
        let who = a(&x["address"]);
        assert_eq!(
            alloy_primitives::keccak256(who).to_string(),
            x["key"].as_str().unwrap(),
            "address preimage"
        );
        flat.set_account(who, Some(acc));
    }
    let root = |f: &MemFlat| {
        trie::root_of_hashed(
            f.accounts()
                .map(|(who, acc)| (alloy_primitives::keccak256(who), acc.rlp()))
                .chain(anonymous.clone()),
        )
    };
    let parent = root(&flat);
    if format!("{parent:#x}") != v["parentStateRoot"].as_str().unwrap() {
        return Err(format!(
            "dump of the parent state hashes to {parent:#x}, header {}",
            v["parentStateRoot"]
        ));
    }
    let mut diff = StateDiff::default();
    if let (Some(who), false) = (tip_signing_wipe(&spec, n), skip_rule) {
        diff.accounts.insert(who, None);
    }
    diff.apply(&mut flat).unwrap();
    let after = root(&flat);
    if format!("{after:#x}") != v["stateRoot"].as_str().unwrap() {
        return Err(format!(
            "state root after the rule {after:#x}, header {}",
            v["stateRoot"]
        ));
    }
    // Neighbouring blocks: the rule does nothing.
    for m in [n - 1, n + 1] {
        if tip_signing_wipe(&spec, m).is_some() {
            return Err(format!("the rule changes state at block {m}"));
        }
    }
    Ok(())
}

#[test]
fn tip_signing_block_state_root_equals_the_archive() {
    check_tip_signing(false).unwrap();
}

/// Named mutant: 0x89 is not self-destructed at `TIPSigning`.
#[test]
fn mutant_no_self_destruct_at_tip_signing_is_detected() {
    assert!(check_tip_signing(true).is_err());
}

type Credit = dyn Fn(
    &XdcRules,
    Option<Address>,
    Address,
    Option<Address>,
    u128,
    u128,
    u64,
) -> Option<(Address, U256)>;

struct FeeMutant<'a> {
    tweak: &'a dyn Fn(&ChainSpec, u64, &mut XdcRules),
    author: &'a dyn Fn(&ChainSpec, u64, Address) -> Address,
    credit: &'a Credit,
}

fn real_fees() -> FeeMutant<'static> {
    FeeMutant {
        tweak: &|_, _, _| {},
        author: &|_, _, x| x,
        credit: &fees::credit,
    }
}

/// The balance changes of the author and of the owner equal the credits of `fees::credit`.
fn check_fees(m: &FeeMutant) -> Result<usize, String> {
    let mut n_ok = 0;
    for v in lines("special/fee_recipient.jsonl") {
        let (n, spec) = (v["number"].as_u64().unwrap(), spec_of(&v));
        let mut r = rules(&spec, n).unwrap();
        (m.tweak)(&spec, n, &mut r);
        let author = (m.author)(&spec, n, a(&v["author"]));
        let owner = v["owner"].as_str().map(|_| a(&v["owner"]));
        let base = v["baseFee"]
            .as_str()
            .map_or(0, |b| u128::from_str_radix(&b[2..], 16).unwrap());
        let mut paid: BTreeMap<Address, U256> = BTreeMap::new();
        for t in v["txs"].as_array().unwrap() {
            let price =
                u128::from_str_radix(t["price"].as_str().unwrap().trim_start_matches("0x"), 16)
                    .unwrap();
            let to = t["to"].as_str().map(|_| a(&t["to"]));
            if let Some((who, amount)) =
                (m.credit)(&r, to, author, owner, price, base, q(&t["gasUsed"]))
            {
                *paid.entry(who).or_default() += amount;
            }
        }
        let bal = v["balances"].as_object().unwrap();
        for (who, c) in bal {
            let (b, af) = (
                U256::from_str(c[0].as_str().unwrap()).unwrap(),
                U256::from_str(c[1].as_str().unwrap()).unwrap(),
            );
            let want = paid
                .get(&Address::from_str(who).unwrap())
                .copied()
                .unwrap_or_default();
            if af.wrapping_sub(b) != want {
                return Err(format!(
                    "block {n}, {who}: archive balance change {}, xone credit {want}",
                    af.wrapping_sub(b)
                ));
            }
        }
        if let Some(x) = paid.keys().find(|x| !bal.contains_key(&format!("{x:#x}"))) {
            return Err(format!(
                "block {n}: xone credits {x}, whose balance the vector does not watch"
            ));
        }
        n_ok += 1;
    }
    Ok(n_ok)
}

#[test]
fn fee_recipient_and_amount_equal_the_archive() {
    let n = check_fees(&real_fees()).unwrap();
    assert!(n >= 60, "{n} blocks");
    eprintln!("{n} fee blocks");
}

/// Named mutant: after `TIPTRC21Fee` the fee goes to the coinbase, not the owner (xOneGo 7f46c92c).
#[test]
fn mutant_fee_to_coinbase_after_tiptrc21fee_is_detected() {
    let m = FeeMutant {
        tweak: &|_, _, r| r.fee_to_owner = false,
        ..real_fees()
    };
    assert!(check_fees(&m).is_err());
}

/// Named mutant: the base fee is burned after `TIPTRC21Fee` (xOneGo#672): the owner gets
/// `gasPrice - baseFee`.
#[test]
fn mutant_base_fee_burned_is_detected() {
    let burn: &Credit = &|r, to, au, ow, price, base, gas| {
        fees::credit(r, to, au, ow, price.saturating_sub(base), base, gas)
    };
    assert!(check_fees(&FeeMutant {
        credit: burn,
        ..real_fees()
    })
    .is_err());
}

/// Named mutant: a V1 block's fee is credited to `header.Coinbase`, which is zero (xOneGo 39ea9b05).
#[test]
fn mutant_v1_fee_to_header_coinbase_is_detected() {
    let zero: &dyn Fn(&ChainSpec, u64, Address) -> Address = &|s, n, au| {
        if n <= s.xdpos.v2.as_ref().unwrap().switch_block {
            Address::ZERO
        } else {
            au
        }
    };
    let m = FeeMutant {
        tweak: &|_, _, r| r.fee_to_owner = false,
        author: zero,
        credit: &fees::credit,
    };
    assert!(check_fees(&m).is_err());
}

/// The owner rule is strictly after `TIPTRC21Fee` (`core/state_transition.go:900`, `> 0`): the block
/// itself still pays the author. The archive has no fee-paying transaction within 1,500 blocks of
/// Apothem's 23,779,191 (checked when the vectors were exported), so this is checked on the rules,
/// and on the vectors only as "the nearest block below pays the author, the nearest above the owner".
fn check_tiptrc21fee_boundary(ge: bool) -> Result<(), String> {
    for net in Network::ALL {
        let spec = net.spec();
        let ForkBlock::At(b) = spec.effective.tip_trc21_fee else {
            continue;
        };
        let owner = |n: u64| {
            let mut r = rules(&spec, n).unwrap();
            if ge {
                r.fee_to_owner = spec.effective.tip_trc21_fee.is_active(n);
            }
            r.fee_to_owner
        };
        if b > 0 && owner(b - 1) {
            return Err(format!("{}: owner rule before the fork block", net.name()));
        }
        if owner(b) {
            return Err(format!(
                "{}: block {b} itself must still pay the author",
                net.name()
            ));
        }
        if !owner(b + 1) {
            return Err(format!(
                "{}: block {} must pay the owner",
                net.name(),
                b + 1
            ));
        }
    }
    let at = match Network::Apothem.spec().effective.tip_trc21_fee {
        ForkBlock::At(b) => b,
        _ => unreachable!(),
    };
    let v = lines("special/fee_recipient.jsonl");
    let below = v
        .iter()
        .filter(|x| x["number"].as_u64().unwrap() < at)
        .count();
    let above = v
        .iter()
        .filter(|x| x["number"].as_u64().unwrap() > at)
        .count();
    if below == 0 || above == 0 {
        return Err("no fee vector on one side of TIPTRC21Fee".into());
    }
    Ok(())
}

#[test]
fn owner_rule_is_strictly_after_tiptrc21fee() {
    check_tiptrc21fee_boundary(false).unwrap();
}

/// Named mutant: the owner rule starts at `TIPTRC21Fee` (`>=`) instead of after it (`>`)
/// (xOneGo#615).
#[test]
fn mutant_tiptrc21fee_ge_instead_of_gt_is_detected() {
    assert!(check_tiptrc21fee_boundary(true).is_err());
}

/// Named mutant: with no registered owner the fee falls back to the author. geth credits nothing.
#[test]
fn mutant_no_owner_falls_back_to_author_is_detected() {
    let fallback: &Credit = &|r, to, au, ow, price, base, gas| {
        fees::credit(r, to, au, Some(ow.unwrap_or(au)), price, base, gas)
    };
    let any_none = lines("special/fee_recipient.jsonl")
        .iter()
        .any(|v| v["owner"].is_null() && v["number"].as_u64().unwrap() > 23_779_191);
    if any_none {
        assert!(check_fees(&FeeMutant {
            credit: fallback,
            ..real_fees()
        })
        .is_err());
    } else {
        eprintln!(
            "no block after TIPTRC21Fee without a registered owner in the sample: not covered"
        );
    }
}

fn check_base_fee(verify: Verify) -> Result<usize, String> {
    let mut n_ok = 0;
    for v in lines("special/base_fee.jsonl") {
        let (n, spec) = (v["number"].as_u64().unwrap(), spec_of(&v));
        let have = v["baseFeePerGas"]
            .as_str()
            .map(|b| U256::from_str(b).unwrap());
        verify(&spec, n, have).map_err(|e| format!("block {n}: {e}"))?;
        if basefee::base_fee_for_block(&spec, n).map(U256::from) != have {
            return Err(format!(
                "block {n}: function {:?}, archive {have:?}",
                basefee::base_fee_for_block(&spec, n)
            ));
        }
        if n > 0 && basefee::child_base_fee(&spec, n - 1).map(U256::from) != have {
            return Err(format!("block {n}: child fee of {} differs", n - 1));
        }
        n_ok += 1;
    }
    Ok(n_ok)
}

#[test]
fn base_fee_equals_the_archive_around_both_tier_boundaries() {
    let n = check_base_fee(&basefee::verify).unwrap();
    let all = lines("special/base_fee.jsonl");
    let spec = Network::Apothem.spec();
    let (ForkBlock::At(e), ForkBlock::At(g)) = (spec.eip1559, spec.gas2500x) else {
        panic!()
    };
    for b in [e - 1, e, e + 1, g - 1, g, g + 1] {
        assert!(all.iter().any(|v| v["number"] == b), "no vector at {b}");
    }
    eprintln!("{n} headers");
}

/// The negative vectors: every wrong header is rejected with the named error.
fn check_negatives(verify: Verify) -> Result<usize, String> {
    let mut n_ok = 0;
    for v in lines("special/base_fee_negative.jsonl") {
        let h = Header::decode(
            &mut alloy_primitives::hex::decode(v["header"].as_str().unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(format!("{:#x}", h.hash()), v["hash"].as_str().unwrap());
        let got = match verify(&spec_of(&v), h.number, h.base_fee) {
            Ok(()) => "accepted".to_string(),
            Err(BaseFeeError::Wrong { .. }) => "BaseFeeError::Wrong".into(),
            Err(BaseFeeError::Unexpected { .. }) => "BaseFeeError::Unexpected".into(),
            Err(BaseFeeError::Missing) => "BaseFeeError::Missing".into(),
        };
        if got != v["mutation"]["expect"].as_str().unwrap() {
            return Err(format!(
                "{}: {got}, expected {}",
                v["id"], v["mutation"]["expect"]
            ));
        }
        n_ok += 1;
    }
    Ok(n_ok)
}

#[test]
fn wrong_base_fee_headers_are_rejected_with_a_typed_error() {
    let n = check_negatives(&basefee::verify).unwrap();
    assert!(n >= 10, "{n} negative vectors");
}

/// Named mutant: the tier is taken from the parent's number, so the fork block still carries the
/// previous tier and the wrong header is accepted (geth `eip1559.go:87-93`).
#[test]
fn mutant_wrong_tier_accepted_is_detected() {
    let parent_tier = |s: &ChainSpec, n: u64, have: Option<U256>| match (
        basefee::base_fee_for_block(s, n - 1),
        have,
    ) {
        (Some(w), Some(h)) if h == U256::from(w) => Ok(()),
        (None, None) => Ok(()),
        _ => Err(BaseFeeError::Missing),
    };
    assert!(check_negatives(&parent_tier).is_err() || check_base_fee(&parent_tier).is_err());
    assert!(check_negatives(&|_, _, _| Ok(())).is_err());
}

#[test]
fn base_fee_is_absent_then_12_5_then_625_gwei_on_every_network() {
    for net in Network::ALL {
        let spec = net.spec();
        if let ForkBlock::At(e) = spec.eip1559 {
            assert_eq!(
                basefee::base_fee_for_block(&spec, e.saturating_sub(1)).is_some(),
                e == 0
            );
            let first = basefee::base_fee_for_block(&spec, e);
            assert!(
                first == Some(basefee::XDC_BASE_FEE)
                    || first == Some(basefee::XDC_BASE_FEE * basefee::GAS_2500X_MULTIPLIER)
            );
        }
        if let ForkBlock::At(g) = spec.gas2500x {
            let tier = basefee::base_fee_for_block(&spec, g);
            if spec.is_eip1559(g) {
                assert_eq!(
                    tier,
                    Some(basefee::XDC_BASE_FEE * basefee::GAS_2500X_MULTIPLIER),
                    "{}",
                    net.name()
                );
            }
        }
    }
}

#[test]
fn xdcx_empty_route_is_the_receiver_window_and_randomize_is_always_evm() {
    use xone_evm::route::{
        is_special_tx, TRADING_STATE, XDCX, XDCX_LENDING, XDCX_LENDING_FINALIZED_TRADE,
    };
    for net in Network::ALL {
        let spec = net.spec();
        let (ForkBlock::At(from), ForkBlock::At(to)) = (
            spec.effective.tip_xdcx,
            spec.effective.tip_xdcx_receiver_disable,
        ) else {
            continue;
        };
        for target in [
            XDCX,
            TRADING_STATE,
            XDCX_LENDING,
            XDCX_LENDING_FINALIZED_TRADE,
        ] {
            let at = |n: u64| route(&rules(&spec, n).unwrap(), Some(target));
            if from < to {
                assert_eq!(at(from), Route::XdcxEmpty, "{}", net.name());
                assert_eq!(at(to - 1), Route::XdcxEmpty);
                assert_eq!(at(to), Route::Evm);
                if from > 0 {
                    assert_eq!(at(from - 1), Route::Evm);
                }
            } else {
                assert_eq!(
                    at(from),
                    Route::Evm,
                    "an empty window never routes ({})",
                    net.name()
                );
            }
        }
        let r = rules(&spec, from).unwrap();
        assert_eq!(route(&r, None), Route::Evm);
        assert_eq!(
            route(&r, Some(xone_evm::route::RANDOMIZE)),
            Route::RandomizeTx
        );
        assert!(is_special_tx(Some(xone_evm::route::RANDOMIZE)));
        assert!(!is_special_tx(Some(XDCX)) && !is_special_tx(None));
    }
}

#[test]
fn system_contract_addresses_agree_between_crates() {
    use xone_state::system_contracts::{randomize_0x90, signer_0x89};
    assert_eq!(xone_evm::route::BLOCK_SIGNERS, signer_0x89::ADDRESS);
    assert_eq!(xone_evm::route::RANDOMIZE, randomize_0x90::ADDRESS);
}
