//! S3 probe: stock reth v2.8.1 / alloy-trie 0.9.9 types against the geth-XDC wide-balance vectors of
//! XA-02 (`spikes/wide-balance/vectors/encoding.json`, printed by the geth-XDC oracle).
//! A value at or above 2^256 is a 33- or 34-byte balance item. Every stock decoder must reject it,
//! and `balance: U256` cannot hold it, so no stock encoder can produce it.

use alloy_primitives::{hex, B256};
use alloy_rlp::{Decodable, Encodable, Header};
use alloy_trie::TrieAccount;
use reth_eth_wire_types::snap::AccountData;

fn vectors() -> Vec<serde_json::Value> {
    serde_json::from_str(include_str!("../../../wide-balance/vectors/encoding.json")).unwrap()
}

/// `AccountData` is `[hash, slim_account]` (reth-eth-wire-types `snap.rs:110-115`).
fn account_data(slim: &[u8]) -> Vec<u8> {
    let hash = B256::repeat_byte(0x42);
    let mut payload = Vec::new();
    hash.encode(&mut payload);
    payload.extend_from_slice(slim);
    let mut out = Vec::new();
    Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

#[test]
fn stock_types_reject_wide_balances() {
    for v in vectors() {
        let name = v["name"].as_str().unwrap();
        let wide = v["is_wide"].as_bool().unwrap();
        let account = hex::decode(v["account_rlp"].as_str().unwrap()).unwrap();
        let slim = hex::decode(v["slim_rlp"].as_str().unwrap()).unwrap();
        let trie = TrieAccount::decode(&mut &account[..]);
        let snap = AccountData::decode(&mut &account_data(&slim)[..]);
        println!(
            "{name:>12} wide={wide:<5} TrieAccount: {:<14} snap AccountData: {}",
            if trie.is_ok() { "decoded" } else { "REJECTED" },
            if snap.is_ok() { "decoded" } else { "REJECTED" }
        );
        assert_eq!(trie.is_ok(), !wide, "{name}: TrieAccount");
        assert_eq!(snap.is_ok(), !wide, "{name}: AccountData");
        if let Ok(t) = trie {
            // a narrow value round-trips byte for byte
            assert_eq!(alloy_rlp::encode(t), account, "{name}");
        }
    }
}
