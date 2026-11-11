//! The `eth` protocol of XDC: versions 100 and 101 and their message set. The two versions share
//! one status ([`status62`]) and one code space ([`codes`]); they differ only in the framing of
//! headers, bodies and receipts ([`Framing`]). Bodies of opaque items (transactions, receipts)
//! are kept as canonical raw RLP ([`Raw`]); their content is checked by the stage that uses them.

use alloy_primitives::{Bytes, B256, U256};
use alloy_rlp::{BufMut, Decodable, Encodable, RlpDecodable, RlpEncodable};
use xone_primitives::{Header, SyncInfo, Timeout, Vote};

pub mod codes;
pub mod status62;
pub mod v100;
pub mod v101;

pub use status62::Status62;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("unknown message code {0:#x}")]
    UnknownCode(u64),
    #[error("trailing bytes after message")]
    Trailing,
    #[error("request id: {0}")]
    RequestId(&'static str),
    #[error("message code {code:#x}: {len} bytes exceeds the cap of {max}")]
    TooLarge { code: u64, len: usize, max: usize },
}

/// eth/100 sends data-plane messages bare, eth/101 puts a request id first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Bare,
    RequestId,
}

/// The ranges of a canonical RLP item that `Raw` accepts nest at most this deep.
const MAX_DEPTH: usize = 32;

/// One RLP item (string or list) kept as the bytes it was received in. Decoding walks the whole
/// item and rejects any non-canonical length or a nesting deeper than 32.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw(pub Bytes);

fn check_item(item: &[u8], depth: usize) -> alloy_rlp::Result<()> {
    if depth > MAX_DEPTH {
        return Err(alloy_rlp::Error::Custom("rlp nesting too deep"));
    }
    let mut b = item;
    let h = alloy_rlp::Header::decode(&mut b)?;
    if h.list {
        let mut p = b;
        while !p.is_empty() {
            let mut probe = p;
            let child = alloy_rlp::Header::decode(&mut probe)?;
            let total = (p.len() - probe.len())
                .checked_add(child.payload_length)
                .ok_or(alloy_rlp::Error::Overflow)?;
            let (one, rest) = p
                .split_at_checked(total)
                .ok_or(alloy_rlp::Error::InputTooShort)?;
            check_item(one, depth + 1)?;
            p = rest;
        }
    }
    Ok(())
}

impl Decodable for Raw {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let mut probe = *buf;
        let h = alloy_rlp::Header::decode(&mut probe)?;
        let total = (buf.len() - probe.len())
            .checked_add(h.payload_length)
            .ok_or(alloy_rlp::Error::Overflow)?;
        let (item, rest) = buf
            .split_at_checked(total)
            .ok_or(alloy_rlp::Error::InputTooShort)?;
        check_item(item, 0)?;
        *buf = rest;
        Ok(Raw(Bytes::copy_from_slice(item)))
    }
}

impl Encodable for Raw {
    fn encode(&self, out: &mut dyn BufMut) {
        out.put_slice(&self.0);
    }
    fn length(&self) -> usize {
        self.0.len()
    }
}

/// Origin of a header query: a hash (32-byte string) or a number (at most 8 bytes)
/// (`protocol.go:239-273`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Hash(B256),
    Number(u64),
}

impl Encodable for Origin {
    fn encode(&self, out: &mut dyn BufMut) {
        match self {
            Origin::Hash(h) => h.encode(out),
            Origin::Number(n) => n.encode(out),
        }
    }
    fn length(&self) -> usize {
        match self {
            Origin::Hash(h) => h.length(),
            Origin::Number(n) => n.length(),
        }
    }
}

impl Decodable for Origin {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let mut probe = *buf;
        let h = alloy_rlp::Header::decode(&mut probe)?;
        match (h.list, h.payload_length) {
            (false, 32) => B256::decode(buf).map(Origin::Hash),
            (false, 0..=8) => u64::decode(buf).map(Origin::Number),
            _ => Err(alloy_rlp::Error::Custom("invalid origin")),
        }
    }
}

/// `GetBlockHeadersRequest` (`protocol.go:226-231`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GetHeaders {
    pub origin: Origin,
    pub amount: u64,
    pub skip: u64,
    pub reverse: bool,
}

/// `NewBlockHashesPacket` entry (`protocol.go:208-211`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct BlockAnnounce {
    pub hash: B256,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct BlockBody {
    pub transactions: Vec<Raw>,
    pub uncles: Vec<Header>,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Block {
    pub header: Header,
    pub transactions: Vec<Raw>,
    pub uncles: Vec<Header>,
}

/// `NewBlockPacket` (`protocol.go:215-218`): the block and the announcer's TD (rank only).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct NewBlock {
    pub block: Block,
    pub td: U256,
}

/// A BFT consensus message (codes 0xe0 to 0xe2). The body is the bare RLP of the type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BftMessage {
    Vote(Vote),
    Timeout(Timeout),
    SyncInfo(SyncInfo),
}

impl BftMessage {
    pub fn code(&self) -> u64 {
        match self {
            BftMessage::Vote(_) => codes::VOTE,
            BftMessage::Timeout(_) => codes::TIMEOUT,
            BftMessage::SyncInfo(_) => codes::SYNC_INFO,
        }
    }

    pub fn encode_body(&self) -> Vec<u8> {
        match self {
            BftMessage::Vote(m) => alloy_rlp::encode(m),
            BftMessage::Timeout(m) => alloy_rlp::encode(m),
            BftMessage::SyncInfo(m) => alloy_rlp::encode(m),
        }
    }

    /// Strict decode of a message body of BFT `code`: one canonical RLP value, nothing after it.
    pub fn decode_body(code: u64, body: &[u8]) -> Result<Self, WireError> {
        Ok(match code {
            codes::VOTE => BftMessage::Vote(exact(body)?),
            codes::TIMEOUT => BftMessage::Timeout(exact(body)?),
            codes::SYNC_INFO => BftMessage::SyncInfo(exact(body)?),
            c => return Err(WireError::UnknownCode(c)),
        })
    }
}

/// Every message of eth/100 and eth/101. `id` is `Some` exactly under [`Framing::RequestId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Status(Status62),
    NewBlockHashes(Vec<BlockAnnounce>),
    Transactions(Vec<Raw>),
    GetBlockHeaders {
        id: Option<u64>,
        query: GetHeaders,
    },
    BlockHeaders {
        id: Option<u64>,
        headers: Vec<Header>,
    },
    GetBlockBodies {
        id: Option<u64>,
        hashes: Vec<B256>,
    },
    BlockBodies {
        id: Option<u64>,
        bodies: Vec<BlockBody>,
    },
    NewBlock(Box<NewBlock>),
    /// `first_index` and `last_incomplete` exist only in eth/101 (eth/70 shape); in eth/100 they
    /// are 0 and false.
    GetReceipts {
        id: Option<u64>,
        first_index: u64,
        hashes: Vec<B256>,
    },
    /// One raw RLP list of receipts per block.
    Receipts {
        id: Option<u64>,
        last_incomplete: bool,
        per_block: Vec<Raw>,
    },
    Bft(BftMessage),
    /// PooledTransactions, GetNodeData, NodeData, BlockRangeUpdate: one canonical RLP item whose
    /// content belongs to a later work item.
    Opaque {
        code: u64,
        item: Raw,
    },
    /// Codes 0x08 and 0x09: DEX order and lending transactions on legacy peers. Geth drains them
    /// without decoding (`handler.go:238-268`); so do we, within the size cap.
    Drained {
        code: u64,
    },
}

impl Message {
    pub fn code(&self) -> u64 {
        match self {
            Message::Status(_) => codes::STATUS,
            Message::NewBlockHashes(_) => codes::NEW_BLOCK_HASHES,
            Message::Transactions(_) => codes::TRANSACTIONS,
            Message::GetBlockHeaders { .. } => codes::GET_BLOCK_HEADERS,
            Message::BlockHeaders { .. } => codes::BLOCK_HEADERS,
            Message::GetBlockBodies { .. } => codes::GET_BLOCK_BODIES,
            Message::BlockBodies { .. } => codes::BLOCK_BODIES,
            Message::NewBlock(_) => codes::NEW_BLOCK,
            Message::GetReceipts { .. } => codes::GET_RECEIPTS,
            Message::Receipts { .. } => codes::RECEIPTS,
            Message::Bft(m) => m.code(),
            Message::Opaque { code, .. } | Message::Drained { code } => *code,
        }
    }
}

/// One RLP value that fills `body` exactly.
pub fn exact<T: Decodable>(body: &[u8]) -> Result<T, WireError> {
    let mut b = body;
    let v = T::decode(&mut b)?;
    if b.is_empty() {
        Ok(v)
    } else {
        Err(WireError::Trailing)
    }
}

fn list(payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 9);
    alloy_rlp::Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend(payload);
    out
}

/// Frame `mid ++ payload`: bare is the payload alone, request-id framing is `[id, mid.., payload]`.
fn frame(
    f: Framing,
    id: Option<u64>,
    mid: &[u8],
    payload: &impl Encodable,
) -> Result<Vec<u8>, WireError> {
    match (f, id) {
        (Framing::Bare, None) => Ok(alloy_rlp::encode(payload)),
        (Framing::RequestId, Some(id)) => {
            let mut p = alloy_rlp::encode(id);
            p.extend_from_slice(mid);
            payload.encode(&mut p);
            Ok(list(p))
        }
        (Framing::Bare, Some(_)) => Err(WireError::RequestId("eth/100 has no request id")),
        (Framing::RequestId, None) => Err(WireError::RequestId("eth/101 needs a request id")),
    }
}

/// Split `[id, rest..]` (eth/101) or pass the body through (eth/100).
fn unframe(f: Framing, body: &[u8]) -> Result<(Option<u64>, &[u8]), WireError> {
    match f {
        Framing::Bare => Ok((None, body)),
        Framing::RequestId => {
            let mut b = body;
            let h = alloy_rlp::Header::decode(&mut b)?;
            if !h.list {
                return Err(alloy_rlp::Error::UnexpectedString.into());
            }
            if b.len() != h.payload_length {
                return Err(WireError::Trailing);
            }
            let id = u64::decode(&mut b)?;
            Ok((Some(id), b))
        }
    }
}

pub fn decode(f: Framing, code: u64, body: &[u8]) -> Result<Message, WireError> {
    use codes::*;
    Ok(match code {
        STATUS => Message::Status(exact(body)?),
        NEW_BLOCK_HASHES => Message::NewBlockHashes(exact(body)?),
        TRANSACTIONS => Message::Transactions(exact(body)?),
        GET_BLOCK_HEADERS => {
            let (id, rest) = unframe(f, body)?;
            Message::GetBlockHeaders {
                id,
                query: exact(rest)?,
            }
        }
        BLOCK_HEADERS => {
            let (id, rest) = unframe(f, body)?;
            Message::BlockHeaders {
                id,
                headers: exact(rest)?,
            }
        }
        GET_BLOCK_BODIES => {
            let (id, rest) = unframe(f, body)?;
            Message::GetBlockBodies {
                id,
                hashes: exact(rest)?,
            }
        }
        BLOCK_BODIES => {
            let (id, rest) = unframe(f, body)?;
            Message::BlockBodies {
                id,
                bodies: exact(rest)?,
            }
        }
        NEW_BLOCK => Message::NewBlock(Box::new(exact(body)?)),
        GET_RECEIPTS => {
            let (id, mut rest) = unframe(f, body)?;
            let first_index = if id.is_some() {
                u64::decode(&mut rest)?
            } else {
                0
            };
            Message::GetReceipts {
                id,
                first_index,
                hashes: exact(rest)?,
            }
        }
        RECEIPTS => {
            let (id, mut rest) = unframe(f, body)?;
            let last_incomplete = if id.is_some() {
                bool::decode(&mut rest)?
            } else {
                false
            };
            Message::Receipts {
                id,
                last_incomplete,
                per_block: exact(rest)?,
            }
        }
        VOTE | TIMEOUT | SYNC_INFO => Message::Bft(BftMessage::decode_body(code, body)?),
        POOLED_TXS | GET_NODE_DATA | NODE_DATA | BLOCK_RANGE_UPDATE => Message::Opaque {
            code,
            item: exact(body)?,
        },
        NEW_POOLED_TX_HASHES | GET_POOLED_TXS => Message::Drained { code },
        c => return Err(WireError::UnknownCode(c)),
    })
}

pub fn encode(f: Framing, msg: &Message) -> Result<(u64, Vec<u8>), WireError> {
    let body = match msg {
        Message::Status(s) => alloy_rlp::encode(s),
        Message::NewBlockHashes(v) => alloy_rlp::encode(v),
        Message::Transactions(v) => alloy_rlp::encode(v),
        Message::GetBlockHeaders { id, query } => frame(f, *id, &[], query)?,
        Message::BlockHeaders { id, headers } => frame(f, *id, &[], headers)?,
        Message::GetBlockBodies { id, hashes } => frame(f, *id, &[], hashes)?,
        Message::BlockBodies { id, bodies } => frame(f, *id, &[], bodies)?,
        Message::NewBlock(b) => alloy_rlp::encode(&**b),
        Message::GetReceipts {
            id,
            first_index,
            hashes,
        } => frame(f, *id, &alloy_rlp::encode(first_index), hashes)?,
        Message::Receipts {
            id,
            last_incomplete,
            per_block,
        } => frame(f, *id, &alloy_rlp::encode(last_incomplete), per_block)?,
        Message::Bft(m) => m.encode_body(),
        Message::Opaque { item, .. } => item.0.to_vec(),
        Message::Drained { code } => return Err(WireError::UnknownCode(*code)),
    };
    let code = msg.code();
    let max = codes::max_size(code);
    if body.len() > max {
        return Err(WireError::TooLarge {
            code,
            len: body.len(),
            max,
        });
    }
    Ok((code, body))
}
