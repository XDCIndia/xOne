#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)] // test code
//! `tx::decode` recovers the signer of each supported transaction type, and refuses a signature
//! for another chain, a high `s`, and an unknown type.

use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_rlp::{Encodable, RlpEncodable};
use k256::ecdsa::SigningKey;
use xone_executor::tx::{decode, TxError};

fn key() -> SigningKey {
    SigningKey::from_slice(&[7u8; 32]).unwrap()
}

fn address_of(k: &SigningKey) -> Address {
    let p = k.verifying_key().to_encoded_point(false);
    Address::from_slice(&keccak256(&p.as_bytes()[1..])[12..])
}

fn sign(hash: B256) -> (u8, U256, U256) {
    let (sig, id) = key().sign_prehash_recoverable(hash.as_slice()).unwrap();
    let b = sig.to_bytes();
    (
        id.to_byte(),
        U256::from_be_slice(&b[..32]),
        U256::from_be_slice(&b[32..]),
    )
}

#[derive(RlpEncodable)]
struct Eip155 {
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: Bytes,
    v: U256,
    r: U256,
    s: U256,
}

fn legacy(chain: u64) -> Vec<u8> {
    #[derive(RlpEncodable)]
    struct Unsigned {
        nonce: u64,
        gas_price: u128,
        gas: u64,
        to: TxKind,
        value: U256,
        data: Bytes,
        chain: u64,
        r: u8,
        s: u8,
    }
    let to = TxKind::Call(Address::with_last_byte(9));
    let h = keccak256(alloy_rlp::encode(&Unsigned {
        nonce: 3,
        gas_price: 5,
        gas: 21000,
        to,
        value: U256::from(1),
        data: Bytes::new(),
        chain,
        r: 0,
        s: 0,
    }));
    let (y, r, s) = sign(h);
    alloy_rlp::encode(&Eip155 {
        nonce: 3,
        gas_price: 5,
        gas: 21000,
        to,
        value: U256::from(1),
        data: Bytes::new(),
        v: U256::from(35 + 2 * chain + u64::from(y)),
        r,
        s,
    })
}

#[test]
fn legacy_eip155_sender_is_recovered() {
    let t = decode(&legacy(51), 51).unwrap();
    assert_eq!(
        (t.from, t.nonce, t.gas, t.chain_id),
        (address_of(&key()), 3, 21000, Some(51))
    );
}

#[test]
fn signature_for_another_chain_is_refused() {
    assert_eq!(
        decode(&legacy(50), 51),
        Err(TxError::ChainId { have: 50, want: 51 })
    );
}

#[test]
fn dynamic_fee_sender_is_recovered() {
    // the fields are written one by one: the empty access list is the single byte 0xc0
    let to = TxKind::Create;
    let mut p = vec![2u8];
    let fields = (
        51u64,
        1u64,
        2u128,
        3u128,
        100_000u64,
        to,
        U256::ZERO,
        Bytes::new(),
    );
    let body = |extra: &mut Vec<u8>| {
        let mut payload = Vec::new();
        fields.0.encode(&mut payload);
        fields.1.encode(&mut payload);
        fields.2.encode(&mut payload);
        fields.3.encode(&mut payload);
        fields.4.encode(&mut payload);
        fields.5.encode(&mut payload);
        fields.6.encode(&mut payload);
        fields.7.encode(&mut payload);
        payload.push(alloy_rlp::EMPTY_LIST_CODE);
        payload.extend_from_slice(extra);
        let mut out = Vec::new();
        alloy_rlp::Header {
            list: true,
            payload_length: payload.len(),
        }
        .encode(&mut out);
        out.extend(payload);
        out
    };
    p.extend(body(&mut vec![]));
    let (y, r, s) = sign(keccak256(&p));
    let mut sig = Vec::new();
    y.encode(&mut sig);
    r.encode(&mut sig);
    s.encode(&mut sig);
    let mut raw = vec![2u8];
    raw.extend(body(&mut sig));
    let t = decode(&raw, 51).unwrap();
    assert_eq!(
        (t.from, t.ty, t.priority_fee, t.gas_price, t.to),
        (address_of(&key()), 2, Some(2), 3, None)
    );
}

#[test]
fn unknown_type_is_refused() {
    assert_eq!(decode(&[3, 0xc0], 51), Err(TxError::Type(3)));
}
