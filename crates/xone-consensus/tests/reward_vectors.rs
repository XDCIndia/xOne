#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The reward vectors of `conformance/vectors/reward/`: checkpoints of the Apothem and net2040
//! archives. Each holds the window as the archive's blocks gave it, the 0x88 owner of every paid
//! node at the parent state, the balance change of the checkpoint block that the archive shows
//! (`archive`, after the transactions' own effect and the fee credit are taken out) and the map
//! that the pinned geth-XDC reward functions return on the same inputs (`oracle`,
//! `crates/xone-consensus/oracle/reward`). The engine must equal both, for every checkpoint.
//! Nothing is typed by hand: `conformance/vectors/reward/README.md`.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::Deserialize;
use xone_chainspec::{apothem, load_named, ChainSpec};
use xone_consensus::chain::{SignTx, SignedBlock};
use xone_consensus::reward::{self, tiered, v1, v2, Holders, RewardMap, RewardWindow, Rule};

#[derive(Deserialize)]
struct Vector {
    id: String,
    network: String,
    kind: String,
    number: u64,
    round: u64,
    start: u64,
    end: u64,
    parent_root: B256,
    nodes: Vec<Address>,
    owners: Vec<Address>,
    /// Indices into `nodes`.
    masternodes: Vec<usize>,
    pool: Vec<Address>,
    /// (number, hash, signer indices into `nodes`) of every rewarded block that is a multiple of 15.
    signed: Vec<(u64, B256, Vec<usize>)>,
    archive: BTreeMap<Address, String>,
    oracle: BTreeMap<Address, String>,
    /// Synthetic vectors only: (voter, stake) of a node, in contract order.
    #[serde(default)]
    voters: BTreeMap<Address, Vec<(Address, String)>>,
}

fn dec(s: &str) -> U256 {
    U256::from_str_radix(s, 10).unwrap()
}

fn spec_of(v: &Vector) -> ChainSpec {
    match v.network.as_str() {
        "apothem" => apothem(),
        "net2040" => {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/vectors/consensus/net2040.forks.json"
            );
            load_named("net2040", &std::fs::read_to_string(path).unwrap()).unwrap()
        }
        n => panic!("network {n}"),
    }
}

/// The reward map of a vector through the engine, with the rule the fork rows pick.
fn run(v: &Vector, spec: &ChainSpec) -> (Rule, RewardMap) {
    let rule = reward::rule(spec, v.number);
    let by_hash: BTreeMap<u64, (B256, &Vec<usize>)> =
        v.signed.iter().map(|(k, h, s)| (*k, (*h, s))).collect();
    let blocks: Vec<SignedBlock> = (v.start..v.number)
        .map(|n| SignedBlock {
            number: n,
            // Only the multiples of 15 are signed for; the other hashes are not read by the count.
            hash: by_hash
                .get(&n)
                .map_or_else(|| keccak256(n.to_be_bytes()), |(h, _)| *h),
            signs: by_hash
                .get(&(n.wrapping_sub(1)))
                .map(|(h, who)| {
                    who.iter()
                        .map(|&s| SignTx {
                            sender: v.nodes[s],
                            signed: *h,
                            ok: true,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let (prot, obs) = match rule {
        Rule::Tiered { .. } => tiered::tiers(tiered::config(spec, v.round).unwrap(), &v.pool),
        _ => (vec![], vec![]),
    };
    let w = RewardWindow::new(
        v.start,
        v.end,
        blocks,
        v.masternodes.iter().map(|&i| v.nodes[i]).collect(),
        prot,
        obs,
    )
    .unwrap();
    let h = Holders::new(
        v.parent_root,
        v.nodes.iter().zip(&v.owners).map(|(n, o)| {
            let vs = v
                .voters
                .get(n)
                .map_or_else(Vec::new, |l| l.iter().map(|(a, c)| (*a, dec(c))).collect());
            (*n, *o, vs)
        }),
    );
    let m = match rule {
        Rule::V1 => v1::rewards(spec, v.number, &w, &h, v.parent_root),
        Rule::V2 => v2::rewards(spec, v.number, &w, &h, v.parent_root),
        Rule::Tiered { .. } => tiered::rewards(
            spec,
            v.number,
            tiered::config(spec, v.round).unwrap(),
            &w,
            &h,
            v.parent_root,
        ),
    }
    .unwrap_or_else(|e| panic!("{}: {e}", v.id));
    (rule, m)
}

fn check(v: &Vector) -> Result<(), String> {
    let spec = spec_of(v);
    let (rule, got) = run(v, &spec);
    let want_rule = match v.kind.as_str() {
        "v1" if v.number >= v2_switch(&spec) => "v2",
        "v1" => "v1",
        "v2" | "v2edge" => "v2",
        "tiered" => "tiered",
        k => k,
    };
    let name = match rule {
        Rule::V1 => "v1",
        Rule::V2 => "v2",
        Rule::Tiered { .. } => "tiered",
    };
    if name != want_rule {
        return Err(format!("rule {name}, vector says {want_rule}"));
    }
    let oracle: RewardMap = v.oracle.iter().map(|(a, s)| (*a, dec(s))).collect();
    if got != oracle {
        return Err(format!(
            "differs from the oracle: engine {got:?}, oracle {oracle:?}"
        ));
    }
    for (a, s) in &v.archive {
        let g = got.get(a).copied().unwrap_or(U256::ZERO);
        if g != dec(s) {
            return Err(format!("{a}: engine {g}, archive {s}"));
        }
    }
    if let Some(a) = got
        .keys()
        .find(|a| !v.archive.is_empty() && !v.archive.contains_key(*a))
    {
        return Err(format!(
            "{a} is paid but the archive vector does not know it"
        ));
    }
    Ok(())
}

fn v2_switch(spec: &ChainSpec) -> u64 {
    spec.xdpos.v2.as_ref().unwrap().switch_block
}

fn vectors(file: &str) -> Vec<Vector> {
    let path = format!(
        "{}/../../conformance/vectors/reward/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn run_file(file: &str) -> usize {
    let vs = vectors(file);
    let failed: Vec<String> = vs
        .iter()
        .filter_map(|v| check(v).err().map(|e| format!("{}: {e}", v.id)))
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} failed:\n{}",
        failed.len(),
        vs.len(),
        failed.join("\n")
    );
    vs.len()
}

#[test]
fn apothem_reward_vectors() {
    let n = run_file("apothem.json");
    assert!(n >= 34, "{n}");
}

/// Made-up windows with voters, a foundation owner and all three tiers; the expected maps are the
/// oracle's. The archive vectors carry no voters, so this is where the voter entries are checked.
#[test]
fn synthetic_reward_vectors() {
    let n = run_file("synthetic.json");
    assert!(n >= 5);
    let vs = vectors("synthetic.json");
    // The voters of the tiered cases are in the map with 0 (the percent is 0) or with their share.
    assert!(vs.iter().all(|v| v.oracle.values().any(|a| a == "0")));
}

#[test]
fn net2040_reward_vectors() {
    // Around its own `tipFixedRewardBlock` 16,200: two checkpoints before it, the first after, two more.
    let n = run_file("net2040.json");
    assert!(n >= 7, "{n}");
}

/// The two Apothem boundaries, each with two checkpoints on both sides, by name.
fn boundary(number: u64) -> Vec<(u64, Rule)> {
    vectors("apothem.json")
        .iter()
        .filter(|v| v.number.abs_diff(number) < 4500)
        .map(|v| (v.number, run(v, &spec_of(v)).0))
        .collect()
}

#[test]
fn tier_boundary_83_600_000() {
    let b = boundary(83_600_000);
    let flat = |n| b.iter().any(|(m, r)| *m == n && *r == Rule::V2);
    let tier = |n| {
        b.iter().any(|(m, r)| {
            *m == n
                && matches!(
                    r,
                    Rule::Tiered {
                        proportional: false
                    }
                )
        })
    };
    assert!(flat(83_599_011) && flat(83_599_875), "{b:?}");
    assert!(
        tier(83_600_502) && tier(83_601_087) && tier(83_601_986),
        "{b:?}"
    );
}

#[test]
fn mutant_proportional_reward_at_84_733_000() {
    let b = boundary(84_733_000);
    let fixed = |n| {
        b.iter().any(|(m, r)| {
            *m == n
                && matches!(
                    r,
                    Rule::Tiered {
                        proportional: false
                    }
                )
        })
    };
    let prop = |n| {
        b.iter()
            .any(|(m, r)| *m == n && matches!(r, Rule::Tiered { proportional: true }))
    };
    assert!(fixed(84_731_521) && fixed(84_732_420), "{b:?}");
    assert!(
        prop(84_733_320) && prop(84_734_201) && prop(84_735_099),
        "{b:?}"
    );
    // The amounts too: the checkpoint at 84,733,320 pays proportionally, so the engine's map
    // equals the archive only with the rule on.
    let v = vectors("apothem.json")
        .into_iter()
        .find(|v| v.number == 84_733_320)
        .unwrap();
    assert_eq!(check(&v), Ok(()));
}

#[test]
fn tier_reward_floats_equal_the_oracle() {
    #[derive(Deserialize)]
    struct Row {
        reward: String,
        amount: String,
    }
    let path = format!(
        "{}/../../conformance/vectors/reward/tier_reward.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let rows: Vec<Row> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(rows.len() >= 10);
    for r in rows {
        let f: f64 = r.reward.parse().unwrap();
        assert_eq!(
            reward::tier_reward(f).unwrap(),
            dec(&r.amount),
            "{}",
            r.reward
        );
    }
}
