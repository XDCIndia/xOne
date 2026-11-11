#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test support: a panic is the failure signal"
)]
//! Blocks of the committed replay vectors, and a sealer that answers with the recorded signatures.

use std::{collections::HashMap, fs::File, io::{BufRead, BufReader}, path::PathBuf};

use alloy_primitives::{Address, Bytes, B256};
use alloy_rlp::{Decodable, Header as RlpHeader};
use flate2::read::GzDecoder;
use serde::Deserialize;
use xone_consensus::{
    unit::{Sealer, UnitError},
    v2::safety::{Intent, Kind},
};
use xone_primitives::{ecrecover, AlgorithmId, Header, Signature};

#[derive(Deserialize)]
struct Line {
    n: u64,
    raw: Bytes,
}

pub struct Block {
    pub header: Header,
    pub txs: Vec<Bytes>,
}

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `[header, [transactions], [uncles]]`; a typed transaction is an RLP string around its encoding.
fn decode(raw: &[u8]) -> Block {
    let mut buf = raw;
    let outer = RlpHeader::decode(&mut buf).unwrap();
    let mut buf = &buf[..outer.payload_length];
    let header = Header::decode(&mut buf).unwrap();
    let list = RlpHeader::decode(&mut buf).unwrap();
    let mut txs = &buf[..list.payload_length];
    let mut out = vec![];
    while !txs.is_empty() {
        let start = txs;
        let item = RlpHeader::decode(&mut txs).unwrap();
        let used = start.len() - txs.len();
        out.push(if item.list {
            Bytes::copy_from_slice(&start[..used + item.payload_length])
        } else {
            Bytes::copy_from_slice(&txs[..item.payload_length])
        });
        txs = &txs[item.payload_length..];
    }
    Block { header, txs: out }
}

/// Blocks `from..=to` of a gzipped replay file, by number.
pub fn replay(file: &str, from: u64, to: u64) -> Vec<Block> {
    let f = File::open(root().join("conformance/vectors/replay").join(file)).unwrap();
    BufReader::new(GzDecoder::new(f))
        .lines()
        .map(|l| serde_json::from_str::<Line>(&l.unwrap()).unwrap())
        .filter(|l| (from..=to).contains(&l.n))
        .map(|l| decode(&l.raw))
        .collect()
}

pub fn addrs(b: &[u8]) -> Vec<Address> {
    b.chunks_exact(20).map(Address::from_slice).collect()
}

/// A sealer that returns a recorded signature and first checks that it is the signature of `who` over the
/// digest it was asked to sign: the digest the producer built is the one the real masternode signed.
pub struct Recorded {
    pub who: Address,
    pub sig: Bytes,
    pub asked: Vec<(Kind, u64, B256)>,
}

impl Recorded {
    pub fn new(who: Address, sig: &[u8]) -> Self {
        Self { who, sig: Bytes::copy_from_slice(sig), asked: vec![] }
    }
    fn answer(&self, digest: &B256) -> Result<Signature, UnitError> {
        let got = ecrecover(digest, &self.sig).map_err(|e| UnitError::Sign(e.to_string()))?;
        assert_eq!(got, self.who, "the digest {digest} is not what {} signed", self.who);
        Ok(Signature { algorithm: AlgorithmId::Secp256k1Keccak, bytes: self.sig.clone() })
    }
}

impl Sealer for Recorded {
    fn address(&self) -> Address {
        self.who
    }
    fn signed_up_to(&self, _: Kind) -> u64 {
        0
    }
    fn sign(&mut self, i: Intent) -> Result<Signature, UnitError> {
        self.asked.push((i.kind, i.round, i.digest));
        self.answer(&i.digest)
    }
    fn cosign_v1(&mut self, number: u64, digest: B256) -> Result<Signature, UnitError> {
        self.asked.push((Kind::SealV1, number, digest));
        self.answer(&digest)
    }
    fn sign_tx(&mut self, _: &B256) -> Result<Signature, UnitError> {
        unreachable!("no transaction is signed here")
    }
}

pub fn by_hash(blocks: &[Block]) -> HashMap<B256, &Block> {
    blocks.iter().map(|b| (b.header.hash(), b)).collect()
}

// ---- throwaway keys and signed transactions (test only) ----

use alloy_primitives::{TxKind, U256};
use alloy_rlp::Encodable;
use k256::ecdsa::SigningKey;
use xone_builder::txpool::PoolTx;

pub struct TestKey(pub SigningKey);

impl TestKey {
    pub fn new(n: u8) -> Self {
        Self(SigningKey::from_slice(&alloy_primitives::keccak256([b't', n])[..]).unwrap())
    }
    pub fn address(&self) -> Address {
        let p = self.0.verifying_key().to_encoded_point(false);
        Address::from_slice(&alloy_primitives::keccak256(&p.as_bytes()[1..])[12..])
    }
    pub fn sign(&self, digest: &B256) -> Signature {
        let (s, id) = self.0.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let mut b = s.to_bytes().to_vec();
        b.push(id.to_byte());
        Signature { algorithm: AlgorithmId::Secp256k1Keccak, bytes: Bytes::from(b) }
    }
    /// An EIP-155 legacy transaction, decoded as the pool decodes it.
    pub fn tx(&self, chain_id: u64, nonce: u64, gas_price: u128, gas: u64, to: Address, data: &[u8]) -> std::sync::Arc<PoolTx> {
        let fields = |p: &mut Vec<u8>| {
            nonce.encode(p);
            gas_price.encode(p);
            gas.encode(p);
            TxKind::Call(to).encode(p);
            U256::ZERO.encode(p);
            Bytes::copy_from_slice(data).encode(p);
        };
        let list = |p: Vec<u8>| {
            let mut o = vec![];
            alloy_rlp::Header { list: true, payload_length: p.len() }.encode(&mut o);
            o.extend(p);
            o
        };
        let mut p = vec![];
        fields(&mut p);
        chain_id.encode(&mut p);
        0u8.encode(&mut p);
        0u8.encode(&mut p);
        let sig = self.sign(&alloy_primitives::keccak256(list(p)));
        let (rs, parity) = (&sig.bytes[..64], u64::from(sig.bytes[64]));
        let mut p = vec![];
        fields(&mut p);
        (35 + 2 * chain_id + parity).encode(&mut p);
        U256::from_be_slice(&rs[..32]).encode(&mut p);
        U256::from_be_slice(&rs[32..]).encode(&mut p);
        PoolTx::decode(&list(p), chain_id).unwrap()
    }
}

/// A sealer over a throwaway key with no safety record: for the tests of the pure rules only.
pub struct KeySealer(pub TestKey);

impl Sealer for KeySealer {
    fn address(&self) -> Address {
        self.0.address()
    }
    fn signed_up_to(&self, _: Kind) -> u64 {
        0
    }
    fn sign(&mut self, i: Intent) -> Result<Signature, UnitError> {
        Ok(self.0.sign(&i.digest))
    }
    fn cosign_v1(&mut self, _: u64, d: B256) -> Result<Signature, UnitError> {
        Ok(self.0.sign(&d))
    }
    fn sign_tx(&mut self, d: &B256) -> Result<Signature, UnitError> {
        Ok(self.0.sign(d))
    }
}
