#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The penalty vectors of `conformance/vectors/penalty/`: V2 epoch switches of the Apothem and
//! net2040 archives. Each holds the blocks of the epoch that ends (the coinbase of each, the hash
//! of every 15th, the sign transactions of the nodes whose release is in question), the
//! `Penalties` of the previous epoch-switch headers, the masternodes of the epoch and the
//! candidates, and the `Penalties` field of the epoch-switch header, which the engine must
//! reproduce. Nothing is typed by hand: `conformance/vectors/penalty/README.md`.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, B256};
use serde::Deserialize;
use xone_chainspec::{apothem, load_named, ChainSpec};
use xone_consensus::chain::{BlockReader, Error, HeaderView, PenaltyHistory, SignTx};
use xone_consensus::penalty::{same_set, v2};

#[derive(Deserialize)]
struct Vector {
    id: String,
    network: String,
    number: u64,
    prev: u64,
    round: u64,
    addrs: Vec<Address>,
    parent_masternodes: Vec<usize>,
    candidates: Vec<usize>,
    history: Vec<Vec<usize>>,
    miners: Vec<usize>,
    hash15: BTreeMap<String, B256>,
    signs: Vec<(u64, usize, u64)>,
    header_penalties: Vec<usize>,
}

struct Chain {
    headers: BTreeMap<B256, HeaderView>,
    bodies: BTreeMap<B256, Vec<SignTx>>,
    history: Vec<Vec<Address>>,
}

impl BlockReader for Chain {
    fn header(&self, h: B256, _: u64) -> Option<HeaderView> {
        self.headers.get(&h).copied()
    }
    fn sign_txs(&self, h: B256, _: u64) -> Option<Vec<SignTx>> {
        self.bodies.get(&h).cloned()
    }
}

impl PenaltyHistory for Chain {
    fn penalties(&self, _: B256, back: usize) -> Option<Vec<Address>> {
        self.history.get(back).cloned()
    }
}

fn spec_of(network: &str) -> ChainSpec {
    match network {
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

fn pick(v: &Vector, ix: &[usize]) -> Vec<Address> {
    ix.iter().map(|&i| v.addrs[i]).collect()
}

/// The synthetic chain of a vector; `with_signs` false leaves every body without sign transactions.
fn chain(v: &Vector, with_signs: bool) -> (Chain, B256) {
    let hash = |n: u64| {
        v.hash15
            .get(&n.to_string())
            .copied()
            .unwrap_or_else(|| keccak256(n.to_be_bytes()))
    };
    let mut c = Chain {
        headers: BTreeMap::new(),
        bodies: BTreeMap::new(),
        history: v.history.iter().map(|l| pick(v, l)).collect(),
    };
    for n in v.prev..v.number {
        let h = HeaderView {
            number: n,
            hash: hash(n),
            parent_hash: hash(n.saturating_sub(1)),
            miner: if n == v.prev {
                Address::ZERO
            } else {
                v.addrs[v.miners[usize::try_from(n - v.prev - 1).unwrap()]]
            },
            epoch_switch: n == v.prev,
        };
        c.headers.insert(h.hash, h);
        c.bodies.insert(h.hash, Vec::new());
    }
    if with_signs {
        for &(blk, who, signed) in &v.signs {
            c.bodies.get_mut(&hash(blk)).unwrap().push(SignTx {
                sender: v.addrs[who],
                signed: hash(signed),
                ok: true,
            });
        }
    }
    (c, hash(v.number - 1))
}

fn derive(v: &Vector, spec: &ChainSpec, with_signs: bool) -> Result<Vec<Address>, Error> {
    let (c, parent) = chain(v, with_signs);
    let (pm, cands) = (pick(v, &v.parent_masternodes), pick(v, &v.candidates));
    v2::penalties(
        spec,
        &c,
        &v2::Input {
            number: v.number,
            parent,
            round: v.round,
            parent_masternodes: &pm,
            candidates: &cands,
        },
    )
}

fn vectors(file: &str) -> Vec<Vector> {
    let path = format!(
        "{}/../../conformance/vectors/penalty/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn check_all(file: &str) -> Vec<Vector> {
    let vs = vectors(file);
    let failed: Vec<String> = vs
        .iter()
        .filter_map(|v| {
            let want = pick(v, &v.header_penalties);
            match derive(v, &spec_of(&v.network), true) {
                Ok(got) if same_set(&got, &want) => None,
                Ok(got) => Some(format!("{}: derived {got:?}, header {want:?}", v.id)),
                Err(e) => Some(format!("{}: {e}", v.id)),
            }
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} failed:\n{}",
        failed.len(),
        vs.len(),
        failed.join("\n")
    );
    vs
}

#[test]
fn apothem_penalties_equal_the_header_penalties_field() {
    let vs = check_all("apothem-v2.json");
    assert!(vs.len() >= 20, "{}", vs.len());
}

#[test]
fn net2040_penalties_equal_the_header_penalties_field() {
    // Around its own `tipUpgradePenaltyBlock` 16,200; the sample has no penalised node.
    let vs = check_all("net2040-v2.json");
    assert!(vs.len() >= 7, "{}", vs.len());
}

/// A real comeback: a node penalised at the previous switch, a candidate again, and not penalised
/// at this switch. It is released by its sign transactions: without them the engine keeps it
/// penalised. And a real stay: the same node situation with no qualifying signature.
#[test]
fn a_real_comeback_and_a_real_stay() {
    let mut comebacks = Vec::new();
    let mut stays = Vec::new();
    for v in vectors("apothem-v2.json") {
        let spec = spec_of(&v.network);
        let after = spec.tip_upgrade_penalty.is_active(v.number);
        let (prev, now, cands) = (
            pick(&v, &v.history[0]),
            pick(&v, &v.header_penalties),
            pick(&v, &v.candidates),
        );
        let bare = derive(&v, &spec, false).unwrap();
        for p in prev.iter().filter(|p| cands.contains(p)) {
            if !now.contains(p) {
                // Released: the engine keeps it out only because of its sign transactions.
                assert!(
                    bare.contains(p),
                    "{}: {p} is released without any sign",
                    v.id
                );
                comebacks.push((v.id.clone(), *p, after));
            } else {
                stays.push((v.id.clone(), *p, after));
            }
        }
    }
    println!("comebacks {comebacks:?}");
    println!("stays {stays:?}");
    assert!(
        comebacks.iter().any(|c| !c.2),
        "no pre-upgrade comeback in the vectors"
    );
    assert!(
        stays.iter().any(|c| !c.2),
        "no pre-upgrade stay in the vectors"
    );
}

// ---- V1 ----

#[derive(Deserialize)]
struct V1Vector {
    id: String,
    number: u64,
    addrs: Vec<Address>,
    signers: Vec<usize>,
    prev_masternodes: Vec<usize>,
    next: Vec<usize>,
    comeback_penalties: Option<Vec<usize>>,
    prior_penalties: Vec<usize>,
    header_penalties: Vec<usize>,
    miners: Vec<usize>,
    hash15: BTreeMap<String, B256>,
    signs: Vec<(u64, usize, u64)>,
}

/// `removeItemFromArray` (`engine_v2/engine.go:3324`): every occurrence goes.
fn without(list: &[Address], remove: &[Address]) -> Vec<Address> {
    list.iter()
        .filter(|a| !remove.contains(a))
        .copied()
        .collect()
}

fn v1_derive(v: &V1Vector) -> Result<(Vec<Address>, Vec<Address>), Error> {
    use xone_consensus::penalty::v1;
    let at = |ix: &[usize]| -> Vec<Address> { ix.iter().map(|&i| v.addrs[i]).collect() };
    let n = v.number;
    let hash = |k: u64| {
        v.hash15
            .get(&k.to_string())
            .copied()
            .unwrap_or_else(|| keccak256(k.to_be_bytes()))
    };
    let mut c = Chain {
        headers: BTreeMap::new(),
        bodies: BTreeMap::new(),
        history: vec![],
    };
    for k in n - 900..n {
        let h = HeaderView {
            number: k,
            hash: hash(k),
            parent_hash: hash(k.saturating_sub(1)),
            miner: if k == n - 900 {
                Address::ZERO
            } else {
                v.addrs[v.miners[usize::try_from(k - (n - 900) - 1).unwrap()]]
            },
            epoch_switch: false,
        };
        c.headers.insert(h.hash, h);
        c.bodies.insert(h.hash, Vec::new());
    }
    for &(blk, who, signed) in &v.signs {
        c.bodies.get_mut(&hash(blk)).unwrap().push(SignTx {
            sender: v.addrs[who],
            signed: hash(signed),
            ok: true,
        });
    }
    let (signers, prev) = (at(&v.signers), at(&v.prev_masternodes));
    let comeback = v.comeback_penalties.as_deref().map(at);
    let p = v1::penalties(
        &apothem(),
        &c,
        &v1::Input {
            number: n,
            parent: hash(n - 1),
            candidates: &signers,
            prev_masternodes: &prev,
            comeback_penalties: comeback.as_deref(),
        },
    )?;
    // Prepare (`xdpos.go:1441-1480`): the penalties and those of the last four checkpoints go out of
    // the signer list; what is left is the list in the checkpoint's `extraData`.
    let next = without(&without(&signers, &p), &at(&v.prior_penalties));
    Ok((p, next))
}

#[test]
fn apothem_v1_checkpoint_lists_equal_the_header_extra_data() {
    let path = format!(
        "{}/../../conformance/vectors/penalty/apothem-v1.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let vs: Vec<V1Vector> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(vs.len() >= 10, "{}", vs.len());
    let failed: Vec<String> = vs
        .iter()
        .filter_map(|v| {
            let at = |ix: &[usize]| -> Vec<Address> { ix.iter().map(|&i| v.addrs[i]).collect() };
            match v1_derive(v) {
                Ok((p, next)) if same_set(&next, &at(&v.next)) => {
                    // A header that records its penalties records the derived ones.
                    let rec = at(&v.header_penalties);
                    (!rec.is_empty() && !same_set(&p, &rec))
                        .then(|| format!("{}: derived {p:?}, header {rec:?}", v.id))
                }
                Ok((_, next)) => Some(format!(
                    "{}: next list {next:?}, header {:?}",
                    v.id,
                    at(&v.next)
                )),
                Err(e) => Some(format!("{}: {e}", v.id)),
            }
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} failed:\n{}",
        failed.len(),
        vs.len(),
        failed.join("\n")
    );
}
