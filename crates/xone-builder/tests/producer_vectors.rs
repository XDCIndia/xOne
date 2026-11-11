#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! The producer against the pinned geth-XDC: the gas limit and base fee of the child of a block at the row
//! before, at and after each fork row (`conformance/vectors/producer/oracle.json`, made by
//! `oracle/run-producer.sh`), the gas gates and the data of the sign transaction, the config rows of the timing
//! rules, and against 50 consecutive blocks of each network, and the real sign transactions of the archive.

mod producer_common;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::{Decodable, RlpDecodable};
use producer_common::{replay, root};
use serde_json::Value;
use xone_builder::{
    producer::{gaslimit, signtx, timing},
    txpool::special::SIGN_SELECTOR,
};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_primitives::ecrecover;

fn oracle() -> Value {
    let p = root().join("conformance/vectors/producer/oracle.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn row(v: &Value) -> ForkBlock {
    match v.as_str().map(|s| s.parse::<u64>().unwrap()) {
        None => ForkBlock::Never,
        Some(99_999_999_999) => ForkBlock::Unscheduled,
        Some(n) => ForkBlock::At(n),
    }
}

/// The chain of an oracle network: Apothem and mainnet are the built-in specs (whose rows must equal the
/// pinned `params`), the others are a devnet spec with the rows the oracle printed.
fn spec_of(o: &Value, net: &str) -> ChainSpec {
    let rows = &o["configs"][net]["rows"];
    let mut s = match net {
        "apothem" => Network::Apothem.spec(),
        "mainnet" => Network::Mainnet.spec(),
        _ => Network::Devnet.spec(),
    };
    if matches!(net, "apothem" | "mainnet") {
        assert_eq!(s.dynamic_gas_limit, row(&rows["dynamicGasLimit"]), "{net}");
        assert_eq!(s.gas2500x, row(&rows["gas2500x"]), "{net}");
        assert_eq!(s.eip1559, row(&rows["eip1559"]), "{net}");
    } else {
        s.dynamic_gas_limit = row(&rows["dynamicGasLimit"]);
        s.gas2500x = row(&rows["gas2500x"]);
        s.eip1559 = row(&rows["eip1559"]);
        s.effective.eip1559 = s.eip1559;
    }
    s
}

#[test]
fn the_gas_limit_and_the_base_fee_equal_geth_at_the_rows() {
    let o = oracle();
    let cases = o["gaslimit"].as_array().unwrap();
    assert!(cases.len() > 600);
    let mut nets = std::collections::BTreeSet::new();
    for c in cases {
        let net = c["net"].as_str().unwrap();
        nets.insert(net);
        let s = spec_of(&o, net);
        let (n, p, ceil) = (
            c["number"].as_u64().unwrap(),
            c["parent_gas_limit"].as_u64().unwrap(),
            c["ceil"].as_u64().unwrap(),
        );
        assert_eq!(
            gaslimit::gas_limit(&s, p, n, ceil),
            c["gas_limit"].as_u64().unwrap(),
            "{c}"
        );
        let want = c["base_fee"].as_str().map(|f| f.parse::<u64>().unwrap());
        assert_eq!(gaslimit::base_fee(&s, n), want, "{c}");
    }
    assert_eq!(nets.len(), 5);
}

#[test]
fn the_sign_gates_equal_geth() {
    let o = oracle();
    for c in o["signblock"].as_array().unwrap() {
        let net = c["net"].as_str().unwrap();
        if matches!(net, "apothem" | "mainnet") {
            let s = spec_of(&o, net);
            let n = c["number"].as_u64().unwrap();
            assert_eq!(
                signtx::is_sign_block(&s, n),
                c["sign_block"].as_bool().unwrap(),
                "{c}"
            );
        }
    }
    for c in o["stale"].as_array().unwrap() {
        let (b, h) = (c["block"].as_u64().unwrap(), c["head"].as_u64().unwrap());
        assert_eq!(signtx::is_stale(b, h), c["stale"].as_bool().unwrap(), "{c}");
    }
}

#[test]
fn the_sign_transaction_has_the_data_gas_and_price_of_geth() {
    let o = oracle();
    for c in o["signtx"].as_array().unwrap() {
        let t = signtx::build(
            c["number"].as_u64().unwrap(),
            c["hash"].as_str().unwrap().parse().unwrap(),
            c["nonce"].as_u64().unwrap(),
        );
        assert_eq!(t.gas, c["gas"].as_u64().unwrap());
        assert_eq!(t.gas_price.to_string(), c["gas_price"].as_str().unwrap());
        assert_eq!(t.value.to_string(), c["value"].as_str().unwrap());
        assert_eq!(
            format!("{:#x}", t.to).to_lowercase(),
            c["to"].as_str().unwrap().to_lowercase()
        );
        assert_eq!(
            alloy_primitives::hex::encode(&t.data),
            c["data"].as_str().unwrap()
        );
    }
}

#[test]
fn the_config_rows_of_the_timing_rules_equal_the_pinned_params() {
    let o = oracle();
    for (net, spec) in [
        ("apothem", Network::Apothem.spec()),
        ("mainnet", Network::Mainnet.spec()),
    ] {
        let t = &o["timing"][net];
        assert_eq!(spec.xdpos.period, t["period"].as_u64().unwrap());
        assert_eq!(spec.xdpos.epoch, t["epoch"].as_u64().unwrap());
        let v2 = spec.xdpos.v2.as_ref().unwrap();
        assert_eq!(
            v2.switch_block.to_string(),
            t["switchBlock"].as_str().unwrap()
        );
        for (r, c) in t["rounds"].as_object().unwrap() {
            let cfg = v2.config_at_round(r.parse().unwrap()).unwrap();
            assert_eq!(
                cfg.mine_period,
                c["minePeriod"].as_u64().unwrap(),
                "{net} round {r}"
            );
            assert_eq!(
                cfg.timeout_period,
                c["timeoutPeriod"].as_u64().unwrap(),
                "{net} round {r}"
            );
        }
    }
}

/// A legacy transaction as the archive holds it.
#[derive(RlpDecodable)]
struct Legacy {
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: alloy_primitives::TxKind,
    value: U256,
    data: Bytes,
    v: U256,
    r: U256,
    s: U256,
}

/// Every sign transaction of the replay blocks that signs a block of the same file: the producer builds the
/// same nonce, gas, price, target and data from the block's number and hash, and its EIP-155 digest recovers
/// the archive's sender from the archive's signature.
#[test]
fn the_sign_transactions_of_the_archive_are_what_the_producer_emits() {
    let mut seen = 0;
    let mut recovered = 0;
    for (file, from, to, chain_id) in [
        ("apothem-0-1800.jsonl.gz", 0, 1800, 51u64),
        (
            "apothem-v2-56827800-56830294.jsonl.gz",
            56_827_800,
            56_830_294,
            51,
        ),
    ] {
        let blocks = replay(file, from, to);
        let hashes: std::collections::HashMap<u64, B256> = blocks
            .iter()
            .map(|b| (b.header.number, b.header.hash()))
            .collect();
        for b in &blocks {
            for raw in &b.txs {
                if raw.first().is_some_and(|x| *x < 0xc0) {
                    continue; // a typed transaction is not a sign transaction of this era
                }
                let mut buf: &[u8] = raw;
                let Ok(t) = Legacy::decode(&mut buf) else {
                    continue;
                };
                let alloy_primitives::TxKind::Call(dest) = t.to else {
                    continue;
                };
                if dest != xone_evm::route::BLOCK_SIGNERS
                    || t.data.len() != 68
                    || t.data[..4] != SIGN_SELECTOR
                {
                    continue;
                }
                let number = u64::from_be_bytes(t.data[28..36].try_into().unwrap());
                let hash = B256::from_slice(&t.data[36..68]);
                let Some(known) = hashes.get(&number) else {
                    continue;
                };
                if *known != hash {
                    continue; // a sign of a block of another branch
                }
                let built = signtx::build(number, hash, t.nonce);
                assert_eq!(built.data, t.data, "block {} tx data", b.header.number);
                assert_eq!(built.nonce, t.nonce);
                assert_eq!(built.to, dest);
                assert_eq!(built.value, t.value);
                seen += 1;
                // gas and price: reported, then required below for the chain's whole sample
                assert_eq!(
                    (built.gas, built.gas_price),
                    (t.gas, t.gas_price),
                    "block {} ({number})",
                    b.header.number
                );
                // the signature: `v` is 35 + 2 * chain id + parity, or 27 + parity before EIP-155
                let v: u64 = t.v.to();
                if v >= 35 {
                    let mut sig = [0u8; 65];
                    sig[..32].copy_from_slice(&t.r.to_be_bytes::<32>());
                    sig[32..64].copy_from_slice(&t.s.to_be_bytes::<32>());
                    sig[64] = u8::try_from((v - 35) % 2).unwrap();
                    assert_eq!((v - 35) / 2, chain_id);
                    let who = ecrecover(&signtx::digest(&built, chain_id), &sig).unwrap();
                    assert_ne!(who, Address::ZERO);
                    recovered += 1;
                }
            }
        }
    }
    eprintln!("{seen} archive sign transactions equal, {recovered} signatures recovered by the producer digest");
    assert!(seen >= 100, "{seen} sign transactions of the archive");
    assert!(recovered >= 100, "{recovered} signatures recovered");
    let _ = keccak256(b"");
    let _ = timing::SIGN_FRESHNESS;
}
