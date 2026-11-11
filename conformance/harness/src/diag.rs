//! Diagnosis of a state mismatch against the archive: `eth_getProof` for every account the
//! replay wrote (first differing account, then field or slot), and the archive's own
//! `prestateTracer` diff of the block for accounts it changed and the replay did not.
//! Diagnosis only: nothing here decides whether a range passes.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use xone_primitives::WideBalance;
use xone_state::StateDiff;
use xone_sweep::fetch::Rpc;

fn q(v: &Value) -> U256 {
    v.as_str()
        .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or_default()
}

pub fn diagnose(rpc: &mut Rpc, block: u64, diff: &StateDiff) -> Result<String, String> {
    let at = format!("{block:#x}");
    let mut out = String::new();
    let addrs: Vec<Address> = diff.accounts.keys().copied().collect();
    let empty_code = keccak256([]);
    'accounts: for chunk in addrs.chunks(5) {
        let calls = chunk
            .iter()
            .map(|a| {
                let keys: Vec<String> = diff
                    .storage
                    .range((*a, B256::ZERO)..=(*a, B256::repeat_byte(0xff)))
                    .map(|((_, k), _)| format!("{k:#x}"))
                    .collect();
                ("eth_getProof", json!([format!("{a:#x}"), keys, at]))
            })
            .collect();
        for (a, ans) in chunk.iter().zip(rpc.batch(calls)?) {
            let r = &ans["result"];
            if r.is_null() {
                return Err(format!("eth_getProof {a}: {}", ans["error"]));
            }
            let (nonce, bal) = (q(&r["nonce"]).to::<u64>(), q(&r["balance"]));
            let code: B256 =
                serde_json::from_value(r["codeHash"].clone()).map_err(|e| e.to_string())?;
            let ours = diff.accounts[a].as_ref();
            let (on, ob, oc) = ours.map_or((0, WideBalance::default(), empty_code), |x| {
                (x.nonce, x.balance.clone(), x.code_hash)
            });
            let mut diffs = vec![];
            if on != nonce {
                diffs.push(format!("nonce: ours {on}, archive {nonce}"));
            }
            if ob.to_u256() != Some(bal) {
                diffs.push(format!("balance: ours {:?}, archive {bal}", ob.to_u256()));
            }
            if oc != code && !(ours.is_none() && code == empty_code) {
                diffs.push(format!("codeHash: ours {oc}, archive {code}"));
            }
            if ours.is_some() {
                for s in r["storageProof"].as_array().into_iter().flatten() {
                    let key: B256 = serde_json::from_value(s["key"].clone()).unwrap_or_default();
                    let have = q(&s["value"]);
                    if let Some(want) = diff.storage.get(&(*a, key)) {
                        if *want != have {
                            diffs.push(format!("slot {key}: ours {want}, archive {have}"));
                        }
                    }
                }
            }
            if !diffs.is_empty() {
                out += &format!(
                    "first differing account {a} (account {}):\n",
                    if ours.is_some() { "written" } else { "deleted" }
                );
                for d in diffs {
                    out += &format!("  - {d}\n");
                }
                break 'accounts;
            }
        }
    }
    if out.is_empty() {
        out += "every account the replay wrote matches the archive's proof at this block\n";
    }
    let t = rpc.batch(vec![(
        "debug_traceBlockByNumber",
        json!([at, {"tracer":"prestateTracer","tracerConfig":{"diffMode":true}}]),
    )])?;
    let mut theirs = BTreeSet::new();
    for tx in t[0]["result"].as_array().into_iter().flatten() {
        for side in ["pre", "post"] {
            for k in tx["result"][side]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
            {
                theirs.insert(k.to_lowercase());
            }
        }
    }
    let ours: BTreeSet<String> = diff.accounts.keys().map(|a| format!("{a:#x}")).collect();
    let missing: Vec<_> = theirs.difference(&ours).collect();
    out += &format!("archive prestateTracer accounts not written by the replay: {missing:?}\n");
    Ok(out)
}
