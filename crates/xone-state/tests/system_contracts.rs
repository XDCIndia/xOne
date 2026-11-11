#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! The 0x88, 0x89 and 0x90 readers against the Apothem archive: each vector holds the answer of
//! `eth_call` on the contract (candidates, owner, cap, voters, signers, secrets, opening) and the
//! storage that call read (`debug_traceCall` `prestateTracer`). The reader runs on that storage and
//! must give the archive's answer, so a wrong slot reads zero and fails.

use alloy_primitives::{Address, B256, U256};
use serde_json::Value;
use std::{collections::BTreeMap, str::FromStr};
use xone_state::{
    system_contracts::{mapping_slot, randomize_0x90, signer_0x89, validator_0x88},
    Account, StateRead,
};

struct Slots(BTreeMap<(Address, B256), U256>);

impl StateRead for Slots {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, a: Address, s: B256) -> U256 {
        self.0.get(&(a, s)).copied().unwrap_or_default()
    }
}

/// Reads shifted by `d` slots: a reader whose slot arithmetic is off by `d` sees this state.
struct Shifted(Slots, u64);

impl StateRead for Shifted {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, a: Address, s: B256) -> U256 {
        let v = U256::from_be_bytes(s.0).wrapping_add(U256::from(self.1));
        self.0.storage(a, B256::from(v.to_be_bytes::<32>()))
    }
}

fn a(v: &Value) -> Address {
    Address::from_str(v.as_str().unwrap()).unwrap()
}
fn word(v: &Value) -> U256 {
    U256::from_str(v.as_str().unwrap()).unwrap_or_default()
}

fn lines() -> Vec<Value> {
    let p = format!(
        "{}/../../conformance/vectors/special/system_contracts.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn state(v: &Value, contract: Address) -> Slots {
    Slots(
        v["slots"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, x)| ((contract, B256::from_str(k).unwrap()), word(x)))
            .collect(),
    )
}

/// `Ok(n)`: the number of archive answers the readers reproduced; `Err`: the first difference.
fn check<S: StateRead>(wrap: impl Fn(Slots) -> S) -> Result<usize, String> {
    let mut n = 0;
    for v in lines() {
        let at = v["number"].as_u64().unwrap();
        let c = match v["contract"].as_str().unwrap() {
            "0x88" => validator_0x88::ADDRESS,
            "0x89" => signer_0x89::ADDRESS,
            _ => randomize_0x90::ADDRESS,
        };
        let r = &wrap(state(&v, c));
        let err = |what: &str, have: String, want: String| {
            Err(format!("block {at}, {what}: reader {have}, archive {want}"))
        };
        match v["contract"].as_str().unwrap() {
            "0x88" => {
                // The contract's getter returns the raw array, deleted candidates as zero entries;
                // geth's `GetCandidates` skips them (`statedb_utils.go:105-109`).
                let want: Vec<Address> = v["candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(a)
                    .filter(|c| !c.is_zero())
                    .collect();
                let have = validator_0x88::candidates(r);
                if have != want {
                    return err("candidates", format!("{have:?}"), format!("{want:?}"));
                }
                n += 1;
                for x in v["reads"].as_array().unwrap() {
                    let cand = a(&x["candidate"]);
                    let want =
                        Address::from_word(B256::from(word(&x["owner"]).to_be_bytes::<32>()));
                    let have = validator_0x88::candidate_owner(r, cand);
                    if have != (!want.is_zero()).then_some(want) {
                        return err("owner", format!("{have:?}"), format!("{want}"));
                    }
                    let (have, want) = (validator_0x88::candidate_cap(r, cand), word(&x["cap"]));
                    if have != want {
                        return err("cap", have.to_string(), want.to_string());
                    }
                    let want: Vec<Address> =
                        x["voters"].as_array().unwrap().iter().map(a).collect();
                    let have = validator_0x88::voters(r, cand);
                    if have != want {
                        return err("voters", format!("{have:?}"), format!("{want:?}"));
                    }
                    if let Some(vc) = x["voterCap"].as_object() {
                        let have = validator_0x88::voter_cap(r, cand, a(&vc["voter"]));
                        if have != word(&vc["cap"]) {
                            return err("voter cap", have.to_string(), vc["cap"].to_string());
                        }
                    }
                    n += 4;
                }
            }
            "0x89" => {
                for x in v["reads"].as_array().unwrap() {
                    let want: Vec<Address> =
                        x["signers"].as_array().unwrap().iter().map(a).collect();
                    let have = signer_0x89::signers(
                        r,
                        B256::from_str(x["blockHash"].as_str().unwrap()).unwrap(),
                    );
                    if have != want {
                        return err("signers", format!("{have:?}"), format!("{want:?}"));
                    }
                    n += 1;
                }
            }
            _ => {
                for x in v["reads"].as_array().unwrap() {
                    let m = a(&x["masternode"]);
                    let want: Vec<B256> = x["secrets"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| B256::from_str(s.as_str().unwrap()).unwrap())
                        .collect();
                    let have = randomize_0x90::secrets(r, m);
                    if have != want {
                        return err("secrets", format!("{have:?}"), format!("{want:?}"));
                    }
                    let want = B256::from(word(&x["opening"]).to_be_bytes::<32>());
                    let have = randomize_0x90::opening(r, m);
                    if have != want {
                        return err("opening", format!("{have}"), format!("{want}"));
                    }
                    n += 2;
                }
            }
        }
    }
    Ok(n)
}

#[test]
fn readers_equal_the_archive() {
    let n = check(|s| s).unwrap();
    let blocks = lines().len();
    assert!(blocks >= 100, "{blocks} sampled reads");
    eprintln!("{blocks} sampled reads, {n} archive answers reproduced");
}

/// Named mutant: a reader whose slot arithmetic is off by one slot reads other storage.
#[test]
fn mutant_slot_off_by_one_is_detected() {
    assert!(check(|s| Shifted(s, 1)).is_err());
}

#[test]
fn mapping_slot_is_keccak_of_key_then_slot() {
    // geth `GetLocMappingAtKey`: keccak256(pad32(key) ++ pad32(slot)).
    let k = B256::repeat_byte(7);
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(k.as_slice());
    b[63] = 3;
    assert_eq!(
        mapping_slot(k, 3),
        U256::from_be_bytes(alloy_primitives::keccak256(b).0)
    );
}
