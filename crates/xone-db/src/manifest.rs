//! Generation marker and segment manifest, as stored in the MDBX `Meta` table.
//!
//! Layout (little endian, 257 bytes), see ADR-005 section 6:
//! `magic "XMF1" | version u16 | generation u64 | head_number u64 | head_hash 32 |
//!  3 x entry | keccak256(all preceding bytes) 32`
//! entry (57 bytes): `segment u8 | first_block u64 | last_block u64 | len u64 | tail_digest 32`.
//! `len` is the end offset of the last committed frame; `tail_digest` is
//! keccak256 of that whole frame.
use alloy_primitives::{keccak256, B256};

pub const MAGIC: [u8; 4] = *b"XMF1";
pub const FORMAT_VERSION: u16 = 1;
/// `Meta` table keys. `gen` holds the generation as u64 LE and must equal the manifest's.
pub const GEN_KEY: &[u8] = b"gen";
pub const MANIFEST_KEY: &[u8] = b"manifest";
const ENTRY_LEN: usize = 57;
const BODY_LEN: usize = 4 + 2 + 8 + 8 + 32 + 3 * ENTRY_LEN;
pub const MANIFEST_LEN: usize = BODY_LEN + 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Headers = 1,
    Bodies = 2,
    Receipts = 3,
}

impl Segment {
    pub const ALL: [Segment; 3] = [Segment::Headers, Segment::Bodies, Segment::Receipts];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentEntry {
    pub segment: Segment,
    pub first_block: u64,
    pub last_block: u64,
    pub len: u64,
    pub tail_digest: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub generation: u64,
    pub head_number: u64,
    pub head_hash: B256,
    /// Order is Headers, Bodies, Receipts.
    pub entries: [SegmentEntry; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic,
    /// Version field differs from [`FORMAT_VERSION`].
    Version(u16),
    BadLength,
    BadChecksum,
    BadLayout,
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("slice is 8 bytes"))
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(MANIFEST_LEN);
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        v.extend_from_slice(&self.generation.to_le_bytes());
        v.extend_from_slice(&self.head_number.to_le_bytes());
        v.extend_from_slice(self.head_hash.as_slice());
        for e in &self.entries {
            v.push(e.segment as u8);
            for n in [e.first_block, e.last_block, e.len] {
                v.extend_from_slice(&n.to_le_bytes());
            }
            v.extend_from_slice(e.tail_digest.as_slice());
        }
        let sum = keccak256(&v);
        v.extend_from_slice(sum.as_slice());
        v
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        if b.len() < 6 || b[..4] != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = u16::from_le_bytes([b[4], b[5]]);
        if version != FORMAT_VERSION {
            return Err(DecodeError::Version(version));
        }
        if b.len() != MANIFEST_LEN {
            return Err(DecodeError::BadLength);
        }
        if keccak256(&b[..BODY_LEN]).as_slice() != &b[BODY_LEN..] {
            return Err(DecodeError::BadChecksum);
        }
        let mut entries = [SegmentEntry {
            segment: Segment::Headers,
            first_block: 0,
            last_block: 0,
            len: 0,
            tail_digest: B256::ZERO,
        }; 3];
        for (i, seg) in Segment::ALL.into_iter().enumerate() {
            let o = 54 + i * ENTRY_LEN;
            if b[o] != seg as u8 {
                return Err(DecodeError::BadLayout);
            }
            entries[i] = SegmentEntry {
                segment: seg,
                first_block: u64_at(b, o + 1),
                last_block: u64_at(b, o + 9),
                len: u64_at(b, o + 17),
                tail_digest: B256::from_slice(&b[o + 25..o + 57]),
            };
        }
        Ok(Manifest {
            generation: u64_at(b, 6),
            head_number: u64_at(b, 14),
            head_hash: B256::from_slice(&b[22..54]),
            entries,
        })
    }
}

#[cfg(test)]
pub(crate) fn sample(generation: u64) -> Manifest {
    let e = |segment, len| SegmentEntry {
        segment,
        first_block: 0,
        last_block: 7,
        len,
        tail_digest: B256::repeat_byte(len as u8),
    };
    Manifest {
        generation,
        head_number: 7,
        head_hash: B256::repeat_byte(0xab),
        entries: [
            e(Segment::Headers, 1000),
            e(Segment::Bodies, 2000),
            e(Segment::Receipts, 3000),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_length() {
        let m = sample(5);
        let b = m.encode();
        assert_eq!(b.len(), MANIFEST_LEN);
        assert_eq!(Manifest::decode(&b), Ok(m));
    }

    #[test]
    fn every_single_byte_flip_is_rejected() {
        let b = sample(5).encode();
        for i in 0..b.len() {
            let mut c = b.clone();
            c[i] ^= 1;
            assert!(Manifest::decode(&c).is_err(), "flip at {i} accepted");
        }
    }

    #[test]
    fn version_and_truncation() {
        let mut b = sample(5).encode();
        b[4] = 2;
        assert_eq!(Manifest::decode(&b), Err(DecodeError::Version(2)));
        assert_eq!(
            Manifest::decode(&sample(5).encode()[..100]),
            Err(DecodeError::BadLength)
        );
    }
}
