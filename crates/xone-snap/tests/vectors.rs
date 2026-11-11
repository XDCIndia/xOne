#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Vectors printed by the geth-XDC oracle (`oracle/run.sh`, pinned in `oracle/PIN`): the snap/1
//! messages, XDC slim accounts with wide balances, the tries and the verdict of
//! `trie.VerifyRangeProof` on valid ranges and on mutations of them.

use alloy_primitives::{hex, Bytes, B256};
use serde_json::Value;
use std::collections::BTreeMap;
use xone_snap::{
    codec::*,
    mpt::Trie,
    proof::{verify_range, ProofError},
};
use xone_state::Account;

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/snap.json")).expect("vectors parse")
}

fn b(v: &Value) -> Bytes {
    Bytes::from(hex::decode(v.as_str().expect("hex string")).expect("hex"))
}

fn h(v: &Value) -> B256 {
    B256::from_slice(&b(v))
}

fn list(v: &Value) -> Vec<Bytes> {
    v.as_array().expect("array").iter().map(b).collect()
}

#[test]
fn messages_are_byte_equal_to_geth() {
    let d = vectors();
    let i = &d["inputs"];
    let m = &d["messages"];
    let root = h(&i["root"]);
    let hashes: Vec<B256> = i["account_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(h)
        .collect();
    let bodies = list(&i["account_bodies"]);
    let accounts: Vec<AccountData> = hashes
        .iter()
        .zip(&bodies)
        .map(|(hash, body)| AccountData {
            hash: *hash,
            body: body.clone(),
        })
        .collect();
    let slots: Vec<StorageData> = i["slot_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .zip(list(&i["slot_bodies"]))
        .map(|(hash, body)| StorageData {
            hash: h(hash),
            body,
        })
        .collect();
    let acc0 = h(&i["acct0"]);
    let code1 = b(&i["code1"]);
    let node_proof = list(&i["account_proof"]);
    let want: Vec<(&str, Message)> = vec![
        (
            "GetAccountRange",
            Message::GetAccountRange(GetAccountRange {
                id: 1,
                root,
                origin: h(&i["origin3"]),
                limit: B256::repeat_byte(0xff),
                bytes: 524288,
            }),
        ),
        (
            "AccountRange",
            Message::AccountRange(AccountRange {
                id: 1,
                accounts,
                proof: node_proof.clone(),
            }),
        ),
        (
            "GetStorageRanges_whole",
            Message::GetStorageRanges(GetStorageRanges {
                id: 2,
                root,
                accounts: vec![acc0, alloy_primitives::keccak256("acct-1")],
                origin: Bytes::new(),
                limit: Bytes::new(),
                bytes: 65536,
            }),
        ),
        (
            "GetStorageRanges_chunk",
            Message::GetStorageRanges(GetStorageRanges {
                id: 3,
                root,
                accounts: vec![acc0],
                origin: b(&i["slot_origin5"]),
                limit: vec![0xff; 32].into(),
                bytes: 65536,
            }),
        ),
        (
            "StorageRanges",
            Message::StorageRanges(StorageRanges {
                id: 3,
                slots: vec![slots],
                proof: list(&i["slot_proof"]),
            }),
        ),
        (
            "StorageRanges_empty",
            Message::StorageRanges(StorageRanges {
                id: 4,
                slots: vec![],
                proof: vec![],
            }),
        ),
        (
            "GetByteCodes",
            Message::GetByteCodes(GetByteCodes {
                id: 5,
                hashes: vec![h(&i["code_hash1"]), h(&i["code_hash2"])],
                bytes: 32768,
            }),
        ),
        (
            "ByteCodes",
            Message::ByteCodes(ByteCodes {
                id: 5,
                codes: vec![code1, Bytes::new()],
            }),
        ),
        (
            "GetTrieNodes",
            Message::GetTrieNodes(GetTrieNodes {
                id: 6,
                root,
                paths: vec![
                    vec![vec![0x00].into()],
                    vec![vec![0x1a].into()],
                    vec![
                        acc0.0.to_vec().into(),
                        vec![0x00, 0x99].into(),
                        vec![0x3f].into(),
                    ],
                ],
                bytes: 4096,
            }),
        ),
        (
            "TrieNodes",
            Message::TrieNodes(TrieNodes {
                id: 6,
                nodes: vec![node_proof[0].clone(), node_proof[1].clone()],
            }),
        ),
    ];
    assert_eq!(
        want.len(),
        m.as_object().unwrap().len(),
        "every oracle message is covered"
    );
    for (name, msg) in &want {
        let bytes = b(&m[name]);
        assert_eq!(
            &msg.encode()[..],
            &bytes[..],
            "{name}: encode differs from geth"
        );
        let code = msg.code();
        assert_eq!(
            &Message::decode(code, &bytes).unwrap_or_else(|e| panic!("{name}: {e}")),
            msg,
            "{name}: decode"
        );
    }
    println!("message vectors: {}", want.len());
}

#[test]
fn wide_accounts_round_trip_through_the_slim_form() {
    let d = vectors();
    let wide = d["wide"].as_array().unwrap();
    for w in wide {
        let name = w["name"].as_str().unwrap();
        let acc = Account::from_slim_rlp(&b(&w["slim"])).unwrap();
        let bal = acc.balance.to_be_bytes();
        let want = b(&w["balance"]);
        let want = want
            .iter()
            .skip_while(|x| **x == 0)
            .copied()
            .collect::<Vec<u8>>();
        assert_eq!(bal, want, "{name}: balance");
        assert_eq!(acc.slim_rlp(), b(&w["slim"]).to_vec(), "{name}: slim bytes");
        assert_eq!(acc.rlp(), b(&w["full"]).to_vec(), "{name}: full bytes");
        assert_eq!(acc.nonce, w["nonce"].as_u64().unwrap());
    }
    // the vectors reach 2^256 and above, up to the 2^264 bound and one past it
    let wide_count = wide
        .iter()
        .filter(|w| {
            Account::from_slim_rlp(&b(&w["slim"]))
                .unwrap()
                .balance
                .is_wide()
        })
        .count();
    assert_eq!(wide_count, 6);
    println!(
        "wide account vectors: {} ({wide_count} above 2^256)",
        wide.len()
    );
}

#[test]
fn non_canonical_rlp_is_rejected() {
    let d = vectors();
    let m = &d["messages"];
    let good = b(&m["GetAccountRange"]);
    assert!(Message::decode(GET_ACCOUNT_RANGE, &good).is_ok());
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // the id 1 as a two-byte string: 0x81 0x01 is not the canonical single byte
    let mut v = good.to_vec();
    v.splice(2..3, [0x81, 0x01]);
    v[1] += 1;
    cases.push(("non-canonical single byte", v));
    // the id with a leading zero
    let mut v = good.to_vec();
    v.splice(2..3, [0x82, 0x00, 0x01]);
    v[1] += 2;
    cases.push(("integer with a leading zero", v));
    // a long-form length where the short form fits (list header 0xf8 0x68 -> 0xf9 0x00 0x68)
    let mut v = good.to_vec();
    assert_eq!(v[0], 0xf8);
    v.splice(0..2, [0xf9, 0x00, v[1]]);
    cases.push(("non-canonical list length", v));
    // a trailing byte after the list, and one inside it
    let mut v = good.to_vec();
    v.push(0x00);
    cases.push(("trailing byte", v));
    let mut v = good.to_vec();
    v.push(0x80);
    v[1] += 1;
    cases.push(("trailing item", v));
    // a root of 31 bytes
    let mut v = good.to_vec();
    v.splice(3..4, [0x9f]);
    v.remove(4 + 31);
    v[1] -= 1;
    cases.push(("31-byte root", v));
    // a string where the list is
    cases.push(("string, not a list", vec![0x83, 1, 2, 3]));
    cases.push(("empty", vec![]));
    for (name, bytes) in cases {
        assert!(
            Message::decode(GET_ACCOUNT_RANGE, &bytes).is_err(),
            "{name} must be refused"
        );
    }
    // storage origin that is neither empty nor 32 bytes
    let s = Message::GetStorageRanges(GetStorageRanges {
        id: 1,
        root: B256::ZERO,
        accounts: vec![],
        origin: vec![1; 5].into(),
        limit: Bytes::new(),
        bytes: 1,
    });
    assert_eq!(
        Message::decode(GET_STORAGE_RANGES, &s.encode()),
        Err(CodecError::BadBound("origin"))
    );
    // unknown code, oversized body, a list over its cap
    assert_eq!(Message::decode(8, &good), Err(CodecError::UnknownCode(8)));
    assert!(matches!(
        Message::decode(0, &vec![0u8; MAX_MESSAGE + 1]),
        Err(CodecError::TooLarge(_))
    ));
    let many = Message::GetByteCodes(GetByteCodes {
        id: 1,
        hashes: vec![B256::ZERO; MAX_REQUEST_ITEMS + 1],
        bytes: 1,
    });
    assert!(matches!(
        Message::decode(GET_BYTE_CODES, &many.encode()),
        Err(CodecError::TooMany { .. })
    ));
    // an account body nested deeper than an account can be
    let deep = Message::AccountRange(AccountRange {
        id: 1,
        accounts: vec![AccountData {
            hash: B256::ZERO,
            body: vec![0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc1, 0xc0].into(),
        }],
        proof: vec![],
    });
    assert!(Message::decode(ACCOUNT_RANGE, &deep.encode()).is_err());
}

fn trie_of(t: &Value) -> (Trie, Vec<B256>, Vec<Bytes>) {
    let keys: Vec<B256> = t["keys"].as_array().unwrap().iter().map(h).collect();
    let vals = list(&t["values"]);
    let leaves: BTreeMap<B256, Vec<u8>> = keys
        .iter()
        .copied()
        .zip(vals.iter().map(|v| v.to_vec()))
        .collect();
    (Trie::build(&leaves), keys, vals)
}

#[test]
fn tries_and_proof_nodes_equal_geth() {
    let d = vectors();
    let i = &d["inputs"];
    let (atrie, akeys, _) = trie_of(&d["accounts"]);
    assert_eq!(atrie.root, h(&d["accounts"]["root"]), "account trie root");
    let (strie, skeys, _) = trie_of(&d["slots"]);
    assert_eq!(strie.root, h(&d["slots"]["root"]), "storage trie root");
    // geth: Prove(entries[3]) then Prove(entries[11]) into a ProofSet, listed in insertion order
    let mut proof = atrie.prove(akeys[3]);
    for n in atrie.prove(akeys[11]) {
        if !proof.contains(&n) {
            proof.push(n);
        }
    }
    assert_eq!(
        proof,
        list(&i["account_proof"]),
        "account proof nodes and their order"
    );
    let mut proof = strie.prove(skeys[5]);
    for n in strie.prove(skeys[8]) {
        if !proof.contains(&n) {
            proof.push(n);
        }
    }
    assert_eq!(
        proof,
        list(&i["slot_proof"]),
        "storage proof nodes and their order"
    );
}

#[test]
fn range_proof_verdicts_equal_geth() {
    let d = vectors();
    let cases = d["ranges"].as_array().unwrap();
    let (mut valid, mut invalid) = (0, 0);
    for c in cases {
        let name = format!(
            "{}/{}",
            c["trie"].as_str().unwrap(),
            c["name"].as_str().unwrap()
        );
        let keys: Vec<B256> = c["keys"].as_array().unwrap().iter().map(h).collect();
        let res = verify_range(
            h(&c["root"]),
            h(&c["origin"]),
            &keys,
            &list(&c["values"]),
            &list(&c["proof"]),
        );
        if c["ok"].as_bool().unwrap() {
            assert_eq!(
                res,
                Ok(c["more"].as_bool().unwrap()),
                "{name}: geth accepts ({})",
                c["err"]
            );
            valid += 1;
        } else {
            assert!(
                res.is_err(),
                "{name}: geth refuses ({}) but we accepted {res:?}",
                c["err"]
            );
            invalid += 1;
        }
    }
    println!(
        "range proof vectors: {} ({valid} accepted, {invalid} refused)",
        cases.len()
    );
    assert!(valid >= 20 && invalid >= 30);
}

/// Spot checks of the error kinds, on the first account range of the vectors.
#[test]
fn proof_failures_are_typed() {
    let d = vectors();
    let get = |name: &str| {
        let c = d["ranges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["trie"] == "account" && c["name"] == name)
            .unwrap();
        let keys: Vec<B256> = c["keys"].as_array().unwrap().iter().map(h).collect();
        verify_range(
            h(&c["root"]),
            h(&c["origin"]),
            &keys,
            &list(&c["values"]),
            &list(&c["proof"]),
        )
    };
    assert!(matches!(
        get("truncated_proof"),
        Err(ProofError::MissingNode(_))
    ));
    assert_eq!(get("leaves_reordered"), Err(ProofError::NotIncreasing));
    assert_eq!(get("empty_value"), Err(ProofError::EmptyValue));
    assert_eq!(get("origin_above_first_key"), Err(ProofError::BeforeOrigin));
    assert_eq!(get("extra_leaf"), Err(ProofError::Mismatch));
    assert_eq!(get("missing_leaf"), Err(ProofError::Mismatch));
    assert_eq!(get("modified_value"), Err(ProofError::Mismatch));
    assert_eq!(get("partial_no_proof_origin_set"), Err(ProofError::NoProof));
    assert_eq!(get("empty_but_more"), Err(ProofError::MoreEntries));
    // a proof that was reordered is still a set of nodes: accepted, as geth does
    assert_eq!(get("proof_reordered"), Ok(true));
}
