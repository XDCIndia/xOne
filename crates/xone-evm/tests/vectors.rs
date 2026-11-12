#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! Rule switches, precompile sets and intrinsic gas against the geth-XDC oracle, at every height a
//! fork row of the generated table can flip at (row - 1, row, row + 1).
//! Vectors: `conformance/vectors/evm/{precompiles,rules}.json`, written by `oracle/run.sh`.

use alloy_primitives::{hex, Address, U256};
use revm::{interpreter::InitialAndFloorGas, primitives::hardfork::SpecId};
use serde::Deserialize;
use std::{collections::BTreeSet, str::FromStr};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_evm::{gas, precompiles, spec};

#[derive(Deserialize)]
struct File<R> {
    pin: String,
    networks: std::collections::BTreeMap<String, Net<R>>,
    #[serde(default)]
    probes: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Net<R> {
    #[serde(rename = "chainId")]
    chain_id: u64,
    rows: Vec<R>,
}
#[derive(Deserialize)]
struct Contract {
    address: String,
    #[serde(rename = "gasProbe")]
    gas_probe: Option<u64>,
}
#[derive(Deserialize)]
struct PcRow {
    number: u64,
    active: Vec<String>,
    executing: Vec<Contract>,
}
#[derive(Deserialize)]
struct Intrinsic {
    case: String,
    gas: u64,
}
#[derive(Deserialize)]
struct RuleRow {
    number: u64,
    flags: std::collections::BTreeMap<String, bool>,
    intrinsic: Vec<Intrinsic>,
}

fn load<R: for<'a> Deserialize<'a>>(name: &str) -> File<R> {
    let path = format!(
        "{}/../../conformance/vectors/evm/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: File<R> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let pin = include_str!("../../../conformance/PIN").trim();
    assert_eq!(
        f.pin, pin,
        "{name} was generated from another geth-XDC commit"
    );
    f
}

/// The four built-in networks and devnet551 with EIP-1559 active at genesis (xOneGo#643).
fn specs() -> Vec<(&'static str, ChainSpec)> {
    let mut at_genesis = Network::Devnet551.spec();
    at_genesis.eip1559 = ForkBlock::At(0);
    let mut v: Vec<_> = Network::ALL.iter().map(|n| (n.name(), n.spec())).collect();
    v.push(("devnet551-eip1559-at-0", at_genesis));
    v
}

fn addrs(v: &[String]) -> BTreeSet<Address> {
    v.iter().map(|a| Address::from_str(a).unwrap()).collect()
}

#[test]
fn precompile_sets_equal_geth_at_every_fork_row() {
    let f: File<PcRow> = load("precompiles.json");
    let (mut rows, mut probed) = (0, 0);
    for (name, spec) in specs() {
        let n = &f.networks[name];
        assert_eq!(n.chain_id, spec.chain_id);
        for r in &n.rows {
            let rules = spec::rules(&spec, r.number).unwrap();
            let row = precompiles::row(&spec, &rules, r.number);
            let ctx = format!("{name} block {}", r.number);
            let want: Vec<String> = r.executing.iter().map(|c| c.address.clone()).collect();
            assert_eq!(row.executing(), addrs(&want), "executing set, {ctx}");
            assert_eq!(row.warm(), addrs(&r.active), "access-list set, {ctx}");
            // Gas of each precompile for the oracle's probe input: tells MODEXP and bn256 variants apart.
            for c in &r.executing {
                let (Some(want), Some(input)) = (c.gas_probe, f.probes.get(&c.address)) else {
                    continue;
                };
                let a = Address::from_str(&c.address).unwrap();
                let out = row.run(&a, &hex::decode(input).unwrap()).unwrap();
                if let Ok(o) = out {
                    if o.status.is_success() {
                        assert_eq!(o.gas_used, want, "gas of {a}, {ctx}");
                        probed += 1;
                    }
                }
            }
            rows += 1;
        }
    }
    eprintln!("{rows} rows, {probed} gas probes");
    assert!(rows >= 60, "{rows} rows");
    assert!(probed >= 300, "{probed} gas probes compared");
}

#[test]
fn rule_flags_equal_geth_at_every_fork_row() {
    let f: File<RuleRow> = load("rules.json");
    for (name, spec) in specs() {
        for r in &f.networks[name].rows {
            let rules = spec::rules(&spec, r.number).unwrap();
            let ctx = format!("{name} block {}", r.number);
            let g = |k: &str| r.flags[k];
            assert_eq!(spec.is_istanbul(r.number), g("istanbul"), "istanbul, {ctx}");
            assert_eq!(
                spec.is_petersburg(r.number),
                g("petersburg"),
                "petersburg, {ctx}"
            );
            assert_eq!(rules.eip1559_bundle, g("eip1559"), "eip1559, {ctx}");
            assert_eq!(rules.eip1559_bundle, g("eip2929"), "eip2929, {ctx}");
            assert_eq!(rules.eip1559_bundle, g("eip3860"), "eip3860, {ctx}");
            assert_eq!(
                rules.prevrandao_is_number_hash,
                g("london"),
                "london, {ctx}"
            );
            assert_eq!(rules.prevrandao_is_number_hash, g("merge"), "merge, {ctx}");
            assert_eq!(
                rules.prevrandao_is_number_hash,
                g("shanghai"),
                "shanghai, {ctx}"
            );
            assert_eq!(
                rules.spec_id >= SpecId::CANCUN,
                g("cancun"),
                "cancun, {ctx}"
            );
            assert_eq!(
                rules.spec_id >= SpecId::PRAGUE,
                g("prague"),
                "prague, {ctx}"
            );
            assert_eq!(
                rules.non_zero_calldata_gas == 68,
                g("preEip2028"),
                "calldata, {ctx}"
            );
            assert_eq!(
                rules.static_call_touch,
                g("istanbul"),
                "static call touch, {ctx}"
            );
            assert_eq!(
                rules.max_refund_quotient == 5,
                g("eip1559"),
                "refund cap, {ctx}"
            );
            // XDCx precompiles stop at TIPXDCXMinerDisable, and Cancun and later never have them.
            let ended = g("xdcxMinerDisable")
                || g("cancun")
                || (spec.berlin.is_active(r.number) && g("eip1559"));
            assert_eq!(
                rules.xdcx_precompiles,
                spec.byzantium.is_active(r.number) && !ended,
                "xdcx, {ctx}"
            );
        }
    }
}

#[test]
fn intrinsic_gas_equals_geth_at_every_fork_row() {
    let f: File<RuleRow> = load("rules.json");
    let mut n_cases = 0;
    for (name, spec) in specs() {
        for r in &f.networks[name].rows {
            let rules = spec::rules(&spec, r.number).unwrap();
            for c in &r.intrinsic {
                let (case, data) = c.case.rsplit_once('-').unwrap();
                // the cases that carry value or send to the sender (EIP-2780 prices them apart)
                let value = if case.ends_with('z')
                    || !(case.starts_with("callv")
                        || case.starts_with("self")
                        || case.starts_with("createv"))
                {
                    U256::ZERO
                } else {
                    U256::from(1u64)
                };
                let g: InitialAndFloorGas = gas::intrinsic_gas(
                    &rules,
                    &hex::decode(data).unwrap(),
                    case.starts_with("create"),
                    0,
                    0,
                    gas::eip2780(&rules, value, case.starts_with("self")),
                );
                assert_eq!(
                    g.initial_total_gas(),
                    c.gas,
                    "{name} block {} case {case}",
                    r.number
                );
                n_cases += 1;
            }
        }
    }
    eprintln!("{n_cases} intrinsic cases");
    assert!(n_cases >= 400, "{n_cases} cases");
}
