//! XDC block header and its RLP encoding.
//!
//! The encoding mirrors geth-XDC `core/types/gen_header_rlp.go`: 15 standard
//! fields, then the XDPoS fields `validators`, `validator`, `penalties` (always
//! written, even when empty), then the optional trailing fields. Optional
//! fields are omitted from the end; a `None` that precedes a `Some` is written
//! as the empty string `0x80`.

mod wide_balance;
pub use wide_balance::{WideBalance, WideBalanceError, MAX_WIDE_BITS};

use alloy_primitives::{keccak256, Address, Bloom, Bytes, B256, B64, U256};
use alloy_rlp::{BufMut, Decodable, Encodable, Error as RlpError};

/// 8-byte block nonce, RLP-encoded as a byte string (not as an integer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockNonce(pub B64);

impl BlockNonce {
    /// Big-endian, as geth `types.EncodeNonce`.
    pub fn from_u64(n: u64) -> Self {
        Self(B64::from(n.to_be_bytes()))
    }
}

/// XDC block header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub parent_hash: B256,
    pub uncle_hash: B256,
    pub coinbase: Address,
    pub state_root: B256,
    pub tx_root: B256,
    pub receipt_root: B256,
    pub bloom: Bloom,
    pub difficulty: U256,
    pub number: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub extra_data: Bytes,
    pub mix_digest: B256,
    pub nonce: BlockNonce,
    // XDPoS fields: always encoded.
    pub validators: Bytes,
    pub validator: Bytes,
    pub penalties: Bytes,
    // Optional trailing fields.
    pub base_fee: Option<U256>,
    pub withdrawals_hash: Option<B256>,
    pub blob_gas_used: Option<u64>,
    pub excess_blob_gas: Option<u64>,
    pub parent_beacon_root: Option<B256>,
    pub requests_hash: Option<B256>,
    pub block_access_list_hash: Option<B256>,
    pub slot_number: Option<u64>,
}

/// An RLP list wrapping `payload`.
fn list(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 9);
    alloy_rlp::Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(payload);
    out
}

fn enc<T: Encodable>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(&mut out);
    out
}

impl Header {
    /// The 15 standard fields plus validators, `validator`, penalties.
    fn core_payload(&self, validator: &Bytes) -> Vec<u8> {
        let mut p = Vec::new();
        self.parent_hash.encode(&mut p);
        self.uncle_hash.encode(&mut p);
        self.coinbase.encode(&mut p);
        self.state_root.encode(&mut p);
        self.tx_root.encode(&mut p);
        self.receipt_root.encode(&mut p);
        self.bloom.encode(&mut p);
        self.difficulty.encode(&mut p);
        self.number.encode(&mut p);
        self.gas_limit.encode(&mut p);
        self.gas_used.encode(&mut p);
        self.timestamp.encode(&mut p);
        self.extra_data.encode(&mut p);
        self.mix_digest.encode(&mut p);
        self.nonce.0.encode(&mut p);
        self.validators.encode(&mut p);
        validator.encode(&mut p);
        self.penalties.encode(&mut p);
        p
    }

    fn rlp_bytes(&self) -> Vec<u8> {
        let mut p = self.core_payload(&self.validator);
        let slots: [Option<Vec<u8>>; 8] = [
            self.base_fee.as_ref().map(enc),
            self.withdrawals_hash.as_ref().map(enc),
            self.blob_gas_used.as_ref().map(enc),
            self.excess_blob_gas.as_ref().map(enc),
            self.parent_beacon_root.as_ref().map(enc),
            self.requests_hash.as_ref().map(enc),
            self.block_access_list_hash.as_ref().map(enc),
            self.slot_number.as_ref().map(enc),
        ];
        if let Some(last) = slots.iter().rposition(Option::is_some) {
            for slot in &slots[..=last] {
                match slot {
                    Some(b) => p.extend_from_slice(b),
                    None => p.push(alloy_rlp::EMPTY_STRING_CODE),
                }
            }
        }
        list(&p)
    }

    /// keccak256 of the RLP encoding.
    pub fn hash(&self) -> B256 {
        keccak256(self.rlp_bytes())
    }

    /// Header hash with an empty `validator` field and no optional fields
    /// (geth-XDC `Header.HashNoValidator`).
    pub fn hash_no_validator(&self) -> B256 {
        keccak256(list(&self.core_payload(&Bytes::new())))
    }
}

impl Encodable for Header {
    fn encode(&self, out: &mut dyn BufMut) {
        out.put_slice(&self.rlp_bytes());
    }
    fn length(&self) -> usize {
        self.rlp_bytes().len()
    }
}

fn is_empty_string(buf: &[u8]) -> bool {
    buf.first() == Some(&alloy_rlp::EMPTY_STRING_CODE)
}

/// Optional hash: the `0x80` placeholder decodes to `None`.
fn opt_hash(buf: &mut &[u8]) -> Result<Option<B256>, RlpError> {
    if is_empty_string(buf) {
        *buf = &buf[1..];
        return Ok(None);
    }
    B256::decode(buf).map(Some)
}

// ponytail: an optional integer cannot tell a `0x80` placeholder from a real
// zero (same as geth), so a leading `None` before a later `Some` decodes as
// `Some(0)`. Real XDC headers never produce that shape.
fn opt_int<T: Decodable>(buf: &mut &[u8]) -> Result<Option<T>, RlpError> {
    T::decode(buf).map(Some)
}

impl Decodable for Header {
    fn decode(buf: &mut &[u8]) -> Result<Self, RlpError> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(RlpError::UnexpectedString);
        }
        if buf.len() < h.payload_length {
            return Err(RlpError::InputTooShort);
        }
        let (mut p, rest) = buf.split_at(h.payload_length);
        *buf = rest;
        let p = &mut p;

        let mut hd = Header {
            parent_hash: Decodable::decode(p)?,
            uncle_hash: Decodable::decode(p)?,
            coinbase: Decodable::decode(p)?,
            state_root: Decodable::decode(p)?,
            tx_root: Decodable::decode(p)?,
            receipt_root: Decodable::decode(p)?,
            bloom: Decodable::decode(p)?,
            difficulty: Decodable::decode(p)?,
            number: Decodable::decode(p)?,
            gas_limit: Decodable::decode(p)?,
            gas_used: Decodable::decode(p)?,
            timestamp: Decodable::decode(p)?,
            extra_data: Decodable::decode(p)?,
            mix_digest: Decodable::decode(p)?,
            nonce: BlockNonce(Decodable::decode(p)?),
            validators: Decodable::decode(p)?,
            validator: Decodable::decode(p)?,
            penalties: Decodable::decode(p)?,
            ..Default::default()
        };
        if !p.is_empty() {
            hd.base_fee = opt_int(p)?;
        }
        if !p.is_empty() {
            hd.withdrawals_hash = opt_hash(p)?;
        }
        if !p.is_empty() {
            hd.blob_gas_used = opt_int(p)?;
        }
        if !p.is_empty() {
            hd.excess_blob_gas = opt_int(p)?;
        }
        if !p.is_empty() {
            hd.parent_beacon_root = opt_hash(p)?;
        }
        if !p.is_empty() {
            hd.requests_hash = opt_hash(p)?;
        }
        if !p.is_empty() {
            hd.block_access_list_hash = opt_hash(p)?;
        }
        if !p.is_empty() {
            hd.slot_number = opt_int(p)?;
        }
        if !p.is_empty() {
            return Err(RlpError::ListLengthMismatch {
                expected: 0,
                got: p.len(),
            });
        }
        Ok(hd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;

    fn sample() -> Header {
        Header {
            parent_hash: b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            difficulty: U256::from(1),
            number: 7,
            gas_limit: 4_712_388,
            timestamp: 0x5d02164f,
            extra_data: Bytes::from_static(&[0xaa; 40]),
            nonce: BlockNonce::from_u64(0x0102),
            validators: Bytes::from_static(&[1, 2, 3]),
            penalties: Bytes::from_static(&[9]),
            ..Default::default()
        }
    }

    /// Top-level items of the encoded header (every field is a byte string).
    fn items(h: &Header) -> Vec<Bytes> {
        let mut out = Vec::new();
        h.encode(&mut out);
        let mut s = out.as_slice();
        let v = Vec::<Bytes>::decode(&mut s).unwrap();
        assert!(s.is_empty());
        v
    }

    #[test]
    fn xdpos_fields_always_encoded() {
        // 15 standard + validators, validator, penalties; validator is empty.
        let it = items(&sample());
        assert_eq!(it.len(), 18);
        assert!(it[16].is_empty());
        assert_eq!(it[14].as_ref(), &[0, 0, 0, 0, 0, 0, 1, 2]);
        // A fully empty header still has the three XDPoS items.
        assert_eq!(items(&Header::default()).len(), 18);
    }

    #[test]
    fn optional_fields_trail_from_the_end() {
        let mut h = sample();
        h.base_fee = Some(U256::from(1_000_000_000u64));
        assert_eq!(items(&h).len(), 19);

        // A later field forces placeholders (0x80) for the earlier None ones.
        h.blob_gas_used = Some(5);
        let it = items(&h);
        assert_eq!(it.len(), 21);
        assert!(it[19].is_empty(), "withdrawals_hash placeholder");
        assert_eq!(it[20].as_ref(), &[5]);

        // Only the last field set: all seven before it are placeholders.
        let mut h = sample();
        h.slot_number = Some(3);
        let it = items(&h);
        assert_eq!(it.len(), 26);
        assert!(it[18..25].iter().all(|b| b.is_empty()));
    }

    #[test]
    fn round_trip() {
        let mut h = sample();
        for step in 0..=8 {
            // Grow the header one trailing field at a time.
            match step {
                1 => h.base_fee = Some(U256::from(1_000_000_000u64)),
                2 => h.withdrawals_hash = Some(B256::repeat_byte(2)),
                3 => h.blob_gas_used = Some(10),
                4 => h.excess_blob_gas = Some(20),
                5 => h.parent_beacon_root = Some(B256::ZERO),
                6 => h.requests_hash = Some(B256::repeat_byte(6)),
                7 => h.block_access_list_hash = Some(B256::repeat_byte(7)),
                8 => h.slot_number = Some(99),
                _ => {}
            }
            let mut out = Vec::new();
            h.encode(&mut out);
            assert_eq!(out.len(), h.length());
            let back = Header::decode(&mut out.as_slice()).unwrap();
            assert_eq!(back, h, "step {step}");
            assert_eq!(back.hash(), h.hash());
        }
    }

    #[test]
    fn hash_no_validator_ignores_validator_and_optionals() {
        let h = sample();
        // Empty validator and no optionals: both hashes cover the same 18 items.
        assert_eq!(h.hash(), h.hash_no_validator());
        let mut signed = h.clone();
        signed.validator = Bytes::from_static(&[0xde; 65]);
        assert_ne!(signed.hash(), h.hash());
        assert_eq!(signed.hash_no_validator(), h.hash_no_validator());
        signed.base_fee = Some(U256::from(1));
        assert_eq!(signed.hash_no_validator(), h.hash_no_validator());
    }
}
