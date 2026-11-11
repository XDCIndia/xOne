//! XDPoS V2 consensus types (geth-XDC `core/types/consensus_v2.go`). Each is an
//! RLP list of its fields in struct order; the decoders reject a list with a
//! missing or extra element and any non-canonical integer.

use alloy_primitives::{Bytes, B256};
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use thiserror::Error;

/// A signature is an opaque byte string at this level; XB-11 checks it.
type Signature = Bytes;

/// `BlockInfo`: block reference used by votes and certificates.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct BlockInfo {
    pub hash: B256,
    pub round: u64,
    pub number: u64,
}

/// `Vote`: the unexported `signer` field is not part of the encoding.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Vote {
    pub proposed_block_info: BlockInfo,
    pub signature: Signature,
    pub gap_number: u64,
}

/// `Timeout`.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Timeout {
    pub round: u64,
    pub signature: Signature,
    pub gap_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct QuorumCert {
    pub proposed_block_info: BlockInfo,
    pub signatures: Vec<Signature>,
    pub gap_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct TimeoutCert {
    pub round: u64,
    pub signatures: Vec<Signature>,
    pub gap_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct SyncInfo {
    pub highest_quorum_cert: QuorumCert,
    pub highest_timeout_cert: TimeoutCert,
}

/// `ExtraFields_v2`. In a header, `extra_data` is the version byte `2` followed
/// by this struct's RLP.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct ExtraFieldsV2 {
    pub round: u64,
    pub quorum_cert: QuorumCert,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtraV2Error {
    #[error("extra data is empty")]
    Empty,
    #[error("consensus version {0} is not 2")]
    Version(u8),
    #[error("{0} trailing bytes after the V2 extra fields")]
    TrailingBytes(usize),
    #[error("invalid V2 extra fields: {0}")]
    Rlp(#[from] alloy_rlp::Error),
}

impl ExtraFieldsV2 {
    /// `DecodeBytesExtraFields`: version byte, then exactly one RLP value.
    pub fn decode_extra(extra: &[u8]) -> Result<Self, ExtraV2Error> {
        let (&version, mut rlp) = extra.split_first().ok_or(ExtraV2Error::Empty)?;
        if version != 2 {
            return Err(ExtraV2Error::Version(version));
        }
        let decoded = Self::decode(&mut rlp)?;
        match rlp.len() {
            0 => Ok(decoded),
            n => Err(ExtraV2Error::TrailingBytes(n)),
        }
    }

    /// `EncodeToBytes`: the `extra_data` of a V2 header.
    pub fn encode_extra(&self) -> Vec<u8> {
        let mut out = vec![2];
        self.encode(&mut out);
        out
    }
}
