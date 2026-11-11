//! Transactions as a block body carries them: decoding and sender recovery.
//!
//! geth `types.Sender` with `MakeSigner(config, number, time)` (`core/types/transaction_signing.go`),
//! for the three types an XDC block can hold before the blob and set-code types: legacy (homestead
//! and EIP-155 signatures), `0x01` access list and `0x02` dynamic fee. The sender is recovered
//! from the signature, never read from an RPC `from` field.

use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use xone_primitives::ecrecover;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TxError {
    #[error("transaction type {0:#x} is not supported")]
    Type(u8),
    #[error("transaction rlp: {0}")]
    Rlp(String),
    #[error("invalid chain id: have {have}, want {want}")]
    ChainId { have: u64, want: u64 },
    #[error("invalid transaction v, r, s values")]
    Signature,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct AccessItem {
    pub address: Address,
    pub keys: Vec<B256>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    pub ty: u8,
    pub chain_id: Option<u64>,
    pub nonce: u64,
    /// Legacy and type 1: the gas price. Type 2: the fee cap.
    pub gas_price: u128,
    /// Type 2 only.
    pub priority_fee: Option<u128>,
    pub gas: u64,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub access_list: Vec<AccessItem>,
    pub from: Address,
}

/// A legacy transaction as sent (`v`, `r`, `s` last), and its signing payloads: homestead (the six
/// fields) and EIP-155 (the six fields, the chain id and two empty items).
#[derive(RlpDecodable)]
struct Legacy {
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

#[derive(RlpEncodable)]
struct LegacyHomestead<'a> {
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: &'a Bytes,
}

#[derive(RlpEncodable)]
struct LegacyEip155<'a> {
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: &'a Bytes,
    chain_id: u64,
    r: u8,
    s: u8,
}

#[derive(RlpDecodable)]
struct T1 {
    chain_id: u64,
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: Bytes,
    access_list: Vec<AccessItem>,
    y: u8,
    r: U256,
    s: U256,
}

#[derive(RlpEncodable)]
struct T1Signing<'a> {
    chain_id: u64,
    nonce: u64,
    gas_price: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: &'a Bytes,
    access_list: &'a Vec<AccessItem>,
}

#[derive(RlpDecodable)]
struct T2 {
    chain_id: u64,
    nonce: u64,
    priority_fee: u128,
    fee_cap: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: Bytes,
    access_list: Vec<AccessItem>,
    y: u8,
    r: U256,
    s: U256,
}

#[derive(RlpEncodable)]
struct T2Signing<'a> {
    chain_id: u64,
    nonce: u64,
    priority_fee: u128,
    fee_cap: u128,
    gas: u64,
    to: TxKind,
    value: U256,
    data: &'a Bytes,
    access_list: &'a Vec<AccessItem>,
}

/// secp256k1 N / 2 (`crypto.secp256k1halfN`): from Homestead a signature with a larger `s` is invalid.
const HALF_N: U256 = U256::from_limbs([
    0xdfe92f46681b20a0,
    0x5d576e7357a4501d,
    0xffffffffffffffff,
    0x7fffffffffffffff,
]);

fn rlp(e: alloy_rlp::Error) -> TxError {
    TxError::Rlp(e.to_string())
}

fn kind(to: TxKind) -> Option<Address> {
    match to {
        TxKind::Call(a) => Some(a),
        TxKind::Create => None,
    }
}

fn recover(hash: B256, y: u8, r: U256, s: U256) -> Result<Address, TxError> {
    // Homestead rules: r and s in [1, N/2], parity 0 or 1 (geth `ValidateSignatureValues`).
    if r.is_zero() || s.is_zero() || s > HALF_N || y > 1 {
        return Err(TxError::Signature);
    }
    let mut sig = [0u8; 65];
    sig[..32].copy_from_slice(&r.to_be_bytes::<32>());
    sig[32..64].copy_from_slice(&s.to_be_bytes::<32>());
    sig[64] = y;
    ecrecover(&hash, &sig).map_err(|_| TxError::Signature)
}

fn typed_hash(ty: u8, payload: &impl Encodable) -> B256 {
    let mut b = vec![ty];
    payload.encode(&mut b);
    keccak256(b)
}

fn check_chain(have: u64, want: u64) -> Result<(), TxError> {
    if have == want {
        Ok(())
    } else {
        Err(TxError::ChainId { have, want })
    }
}

/// Decode a consensus-encoded transaction (legacy: the RLP list; typed: type byte and payload)
/// and recover its sender. `chain_id` is the chain's; a signature for another one is refused.
pub fn decode(raw: &[u8], chain_id: u64) -> Result<Tx, TxError> {
    match raw.first() {
        Some(0xc0..) => legacy(raw, chain_id),
        Some(1) => {
            let t = T1::decode(&mut &raw[1..]).map_err(rlp)?;
            check_chain(t.chain_id, chain_id)?;
            let hash = typed_hash(
                1,
                &T1Signing {
                    chain_id: t.chain_id,
                    nonce: t.nonce,
                    gas_price: t.gas_price,
                    gas: t.gas,
                    to: t.to,
                    value: t.value,
                    data: &t.data,
                    access_list: &t.access_list,
                },
            );
            Ok(Tx {
                ty: 1,
                chain_id: Some(t.chain_id),
                nonce: t.nonce,
                gas_price: t.gas_price,
                priority_fee: None,
                gas: t.gas,
                to: kind(t.to),
                value: t.value,
                from: recover(hash, t.y, t.r, t.s)?,
                data: t.data,
                access_list: t.access_list,
            })
        }
        Some(2) => {
            let t = T2::decode(&mut &raw[1..]).map_err(rlp)?;
            check_chain(t.chain_id, chain_id)?;
            let hash = typed_hash(
                2,
                &T2Signing {
                    chain_id: t.chain_id,
                    nonce: t.nonce,
                    priority_fee: t.priority_fee,
                    fee_cap: t.fee_cap,
                    gas: t.gas,
                    to: t.to,
                    value: t.value,
                    data: &t.data,
                    access_list: &t.access_list,
                },
            );
            Ok(Tx {
                ty: 2,
                chain_id: Some(t.chain_id),
                nonce: t.nonce,
                gas_price: t.fee_cap,
                priority_fee: Some(t.priority_fee),
                gas: t.gas,
                to: kind(t.to),
                value: t.value,
                from: recover(hash, t.y, t.r, t.s)?,
                data: t.data,
                access_list: t.access_list,
            })
        }
        Some(t) => Err(TxError::Type(*t)),
        None => Err(TxError::Rlp("empty".into())),
    }
}

fn legacy(raw: &[u8], chain_id: u64) -> Result<Tx, TxError> {
    let mut buf = raw;
    let t = Legacy::decode(&mut buf).map_err(rlp)?;
    if !buf.is_empty() {
        return Err(TxError::Rlp("trailing bytes".into()));
    }
    let (hash, y, chain) = if t.v == U256::from(27) || t.v == U256::from(28) {
        let h = LegacyHomestead {
            nonce: t.nonce,
            gas_price: t.gas_price,
            gas: t.gas,
            to: t.to,
            value: t.value,
            data: &t.data,
        };
        (
            keccak256(alloy_rlp::encode(&h)),
            u8::from(t.v == U256::from(28)),
            None,
        )
    } else if t.v >= U256::from(35) {
        // geth `deriveChainId`, `SignatureValues`: v = 35 + 2 * chainId + parity
        let w = t.v - U256::from(35);
        let id = u64::try_from(w >> 1).map_err(|_| TxError::Signature)?;
        check_chain(id, chain_id)?;
        let h = LegacyEip155 {
            nonce: t.nonce,
            gas_price: t.gas_price,
            gas: t.gas,
            to: t.to,
            value: t.value,
            data: &t.data,
            chain_id: id,
            r: 0,
            s: 0,
        };
        (
            keccak256(alloy_rlp::encode(&h)),
            u8::from(w.bit(0)),
            Some(id),
        )
    } else {
        return Err(TxError::Signature);
    };
    Ok(Tx {
        ty: 0,
        chain_id: chain,
        nonce: t.nonce,
        gas_price: t.gas_price,
        priority_fee: None,
        gas: t.gas,
        to: kind(t.to),
        value: t.value,
        from: recover(hash, y, t.r, t.s)?,
        data: t.data,
        access_list: vec![],
    })
}
