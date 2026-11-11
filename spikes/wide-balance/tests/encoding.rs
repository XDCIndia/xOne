//! Account leaf, snap slim item and one-account trie root equal geth-XDC for the boundary
//! values, using only an encoder written here: no alloy-trie `TrieAccount`, no revm type.

use alloy_primitives::{keccak256, Address, B256};
use alloy_trie::{HashBuilder, Nibbles, EMPTY_ROOT_HASH, KECCAK_EMPTY};
use serde::Deserialize;
use wide_balance_spike::harness::{encode_account, encode_slim};
use xone_primitives::WideBalance;

#[derive(Deserialize)]
struct Vector {
    name: String,
    r#true: String,
    account_rlp: String,
    slim_rlp: String,
    trie_root: String,
}

fn hex(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap();
    let s = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_string()
    };
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn leaf_slim_and_root_match_go() {
    let root: B256 = "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
        .parse()
        .unwrap();
    let code: B256 = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        .parse()
        .unwrap();
    assert_eq!(code, KECCAK_EMPTY);
    let addr: Address = "0x4398241671b3dd484fe3213a4fb7511f30e7d7c0"
        .parse()
        .unwrap();
    let vs: Vec<Vector> = serde_json::from_str(include_str!("../vectors/encoding.json")).unwrap();
    assert_eq!(vs.len(), 8);
    for v in vs {
        let w = WideBalance::from_be_bytes(&hex(&v.r#true));
        let leaf = encode_account(7, &w, root, code);
        assert_eq!(leaf, hex(&v.account_rlp), "leaf {}", v.name);
        assert_eq!(
            encode_slim(7, &w, root, code),
            hex(&v.slim_rlp),
            "slim {}",
            v.name
        );
        // The empty-root form of the slim item uses the empty string, as geth does.
        assert!(encode_slim(7, &w, EMPTY_ROOT_HASH, code).len() < leaf.len());
        let mut hb = HashBuilder::default();
        hb.add_leaf(Nibbles::unpack(keccak256(addr)), &leaf);
        assert_eq!(
            format!("{:?}", hb.root()),
            v.trie_root,
            "trie root {}",
            v.name
        );
    }
}
