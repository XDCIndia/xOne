//! xpre/1: xOne's preimage extension next to snap/1 (`spec/snap/preimages.md`).
//!
//! snap/1 carries state by hashed key (`keccak(address)`, `keccak(slot)`) and no preimages, while
//! the xOne state is keyed by address (ADR-005 `PlainAccountState`). A node that downloads state
//! asks an xOne peer for the address and slot behind each hash. A preimage proves itself: the
//! client keeps it only if it hashes to the requested key, so a peer can withhold, never forge.
//! geth has no such capability: an xOne node snap-syncs only from xOne peers that offer it.
//!
//! Two messages, each an RLP list that starts with the request id, codes relative to xpre/1.

use alloy_primitives::{Bytes, B256};
use alloy_rlp::{RlpDecodable, RlpEncodable};

use crate::codec::{CodecError, MAX_ITEMS, MAX_MESSAGE, MAX_REQUEST_ITEMS};

pub const GET_PREIMAGES: u64 = 0x00;
pub const PREIMAGES: u64 = 0x01;
/// Hashes a client asks for at once, and a server answers at most (geth `maxCodeLookups`).
pub const MAX_PREIMAGES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GetPreimages {
    pub id: u64,
    pub hashes: Vec<B256>,
}

/// The known preimages of the requested hashes, in request order; an unknown one is skipped.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Preimages {
    pub id: u64,
    pub preimages: Vec<Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Get(GetPreimages),
    Answer(Preimages),
}

impl Message {
    pub fn code(&self) -> u64 {
        match self {
            Self::Get(_) => GET_PREIMAGES,
            Self::Answer(_) => PREIMAGES,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::Get(m) => m.id,
            Self::Answer(m) => m.id,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Get(m) => alloy_rlp::encode(m),
            Self::Answer(m) => alloy_rlp::encode(m),
        }
    }

    /// Strict: one canonical RLP value, nothing after it, lists under their caps.
    pub fn decode(code: u64, body: &[u8]) -> Result<Self, CodecError> {
        if body.len() > MAX_MESSAGE {
            return Err(CodecError::TooLarge(body.len()));
        }
        let cap = |what, len, max| {
            if len > max {
                Err(CodecError::TooMany { what, len, max })
            } else {
                Ok(())
            }
        };
        Ok(match code {
            GET_PREIMAGES => {
                let m: GetPreimages = alloy_rlp::decode_exact(body)?;
                cap("hashes", m.hashes.len(), MAX_REQUEST_ITEMS)?;
                Self::Get(m)
            }
            PREIMAGES => {
                let m: Preimages = alloy_rlp::decode_exact(body)?;
                cap("preimages", m.preimages.len(), MAX_ITEMS)?;
                Self::Answer(m)
            }
            c => return Err(CodecError::UnknownCode(c)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_strictness() {
        let g = Message::Get(GetPreimages {
            id: 7,
            hashes: vec![B256::repeat_byte(1), B256::repeat_byte(2)],
        });
        assert_eq!(Message::decode(GET_PREIMAGES, &g.encode()).unwrap(), g);
        let a = Message::Answer(Preimages {
            id: 7,
            preimages: vec![Bytes::from(vec![9u8; 20])],
        });
        assert_eq!(Message::decode(PREIMAGES, &a.encode()).unwrap(), a);
        let mut trailing = a.encode();
        trailing.push(0x80);
        assert!(Message::decode(PREIMAGES, &trailing).is_err());
        assert!(Message::decode(2, &a.encode()).is_err());
    }
}
