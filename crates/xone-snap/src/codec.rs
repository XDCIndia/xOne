//! The eight snap/1 messages, byte for byte as geth-XDC encodes them
//! (`eth/protocols/snap/protocol.go:51-60` codes, `:77-220` packets). Every message is an RLP list
//! that starts with the request id. Decoding is strict: a non-canonical length or integer, a
//! trailing byte, a hash that is not 32 bytes, or a list longer than its cap is an error (geth
//! reads the same lists with `rlp.RawList`, which refuses non-canonical RLP too).
//!
//! Codes here are snap-relative. On the wire they sit after the eth messages: the RLPx offset is
//! 16 + the eth length (229 for eth/100 and eth/101, `eth/protocols/eth/protocol.go:89`), see
//! `spec/snap/messages.md`.

use alloy_primitives::{Bytes, B256};
use alloy_rlp::{Encodable, Header};

pub const GET_ACCOUNT_RANGE: u64 = 0x00;
pub const ACCOUNT_RANGE: u64 = 0x01;
pub const GET_STORAGE_RANGES: u64 = 0x02;
pub const STORAGE_RANGES: u64 = 0x03;
pub const GET_BYTE_CODES: u64 = 0x04;
pub const BYTE_CODES: u64 = 0x05;
pub const GET_TRIE_NODES: u64 = 0x06;
pub const TRIE_NODES: u64 = 0x07;
/// snap/1 has eight messages (`protocol.go:45`, `protocolLengths`).
pub const MESSAGE_COUNT: u64 = 8;

/// geth `maxMessageSize` (`protocol.go:48`): a decoded body is never larger.
pub const MAX_MESSAGE: usize = 10 * 1024 * 1024;
/// geth `softResponseLimit` (`handler.go:32`): the server caps every `bytes` field at this.
pub const SOFT_RESPONSE_LIMIT: u64 = 2 * 1024 * 1024;
/// geth `maxCodeLookups` and `maxTrieNodeLookups` (`handler.go:36,45`).
pub const MAX_LOOKUPS: usize = 1024;
/// Proof nodes in one response (`handlers.go:128,301`: more than 128 is refused).
pub const MAX_PROOF_NODES: usize = 128;
/// Cap on a list we receive, so that 10 MiB of empty strings cannot allocate 10 M items. It is
/// above what a 2 MiB response can hold (a slim account and its hash are at least 37 bytes).
/// An xOne rule: geth has no count cap, it bounds by bytes only.
pub const MAX_ITEMS: usize = 1 << 17;
/// Cap on the lists of a request (hashes, accounts, paths). geth clients send at most 1,024
/// (`sync.go:66-72`) and geth servers serve at most 1,024 (`handler.go:36,45`).
pub const MAX_REQUEST_ITEMS: usize = 4096;
/// Nesting depth that an account body (one list of four strings) can reach.
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("trailing bytes after the message")]
    Trailing,
    #[error("unknown snap message code {0:#x}")]
    UnknownCode(u64),
    #[error("{what}: {len} items exceeds the cap of {max}")]
    TooMany {
        what: &'static str,
        len: usize,
        max: usize,
    },
    #[error("{0} is neither empty nor 32 bytes")]
    BadBound(&'static str),
    #[error("message body of {0} bytes exceeds {MAX_MESSAGE}")]
    TooLarge(usize),
}

/// `GetAccountRangePacket` (`protocol.go:77-83`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetAccountRange {
    pub id: u64,
    pub root: B256,
    pub origin: B256,
    pub limit: B256,
    pub bytes: u64,
}

/// `AccountData` (`protocol.go:99-102`): `body` is the slim account as one RLP item, kept as
/// received. [`xone_state::Account::from_slim_rlp`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountData {
    pub hash: B256,
    pub body: Bytes,
}

/// `AccountRangePacket` (`protocol.go:92-96`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRange {
    pub id: u64,
    pub accounts: Vec<AccountData>,
    pub proof: Vec<Bytes>,
}

/// `GetStorageRangesPacket` (`protocol.go:127-134`). `origin` and `limit` are empty (the whole
/// trie) or 32 bytes (the first account only, "large contract mode").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetStorageRanges {
    pub id: u64,
    pub root: B256,
    pub accounts: Vec<B256>,
    pub origin: Bytes,
    pub limit: Bytes,
    pub bytes: u64,
}

/// `StorageData` (`protocol.go:150-153`): `body` is the RLP of the trimmed value, the trie leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageData {
    pub hash: B256,
    pub body: Bytes,
}

/// `StorageRangesPacket` (`protocol.go:143-147`): `proof` is for the last range only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRanges {
    pub id: u64,
    pub slots: Vec<Vec<StorageData>>,
    pub proof: Vec<Bytes>,
}

/// `GetByteCodesPacket` (`protocol.go:174-178`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetByteCodes {
    pub id: u64,
    pub hashes: Vec<B256>,
    pub bytes: u64,
}

/// `ByteCodesPacket` (`protocol.go:186-189`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteCodes {
    pub id: u64,
    pub codes: Vec<Bytes>,
}

/// `GetTrieNodesPacket` (`protocol.go:192-197`). Each path set is `[account path]` (a node of
/// the account trie) or `[account hash, storage path..]` (nodes of that account's storage trie);
/// paths are hex-prefix ("compact") encoded (`protocol.go:199-209`, `trie/sync.go:71-102`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetTrieNodes {
    pub id: u64,
    pub root: B256,
    pub paths: Vec<Vec<Bytes>>,
    pub bytes: u64,
}

/// `TrieNodesPacket` (`protocol.go:217-220`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrieNodes {
    pub id: u64,
    pub nodes: Vec<Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    GetAccountRange(GetAccountRange),
    AccountRange(AccountRange),
    GetStorageRanges(GetStorageRanges),
    StorageRanges(StorageRanges),
    GetByteCodes(GetByteCodes),
    ByteCodes(ByteCodes),
    GetTrieNodes(GetTrieNodes),
    TrieNodes(TrieNodes),
}

impl Message {
    pub fn code(&self) -> u64 {
        match self {
            Self::GetAccountRange(_) => GET_ACCOUNT_RANGE,
            Self::AccountRange(_) => ACCOUNT_RANGE,
            Self::GetStorageRanges(_) => GET_STORAGE_RANGES,
            Self::StorageRanges(_) => STORAGE_RANGES,
            Self::GetByteCodes(_) => GET_BYTE_CODES,
            Self::ByteCodes(_) => BYTE_CODES,
            Self::GetTrieNodes(_) => GET_TRIE_NODES,
            Self::TrieNodes(_) => TRIE_NODES,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::GetAccountRange(m) => m.id,
            Self::AccountRange(m) => m.id,
            Self::GetStorageRanges(m) => m.id,
            Self::StorageRanges(m) => m.id,
            Self::GetByteCodes(m) => m.id,
            Self::ByteCodes(m) => m.id,
            Self::GetTrieNodes(m) => m.id,
            Self::TrieNodes(m) => m.id,
        }
    }

    /// The RLP body that goes after the message code.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::GetAccountRange(m) => list(|p| {
                m.id.encode(p);
                m.root.encode(p);
                m.origin.encode(p);
                m.limit.encode(p);
                m.bytes.encode(p);
            }),
            Self::AccountRange(m) => list(|p| {
                m.id.encode(p);
                put_list(p, &m.accounts, |p, a| {
                    list_into(p, |p| {
                        a.hash.encode(p);
                        p.extend_from_slice(&a.body); // the body is already an RLP item
                    })
                });
                put_list(p, &m.proof, |p, n| n.encode(p));
            }),
            Self::GetStorageRanges(m) => list(|p| {
                m.id.encode(p);
                m.root.encode(p);
                put_list(p, &m.accounts, |p, h| h.encode(p));
                m.origin.encode(p);
                m.limit.encode(p);
                m.bytes.encode(p);
            }),
            Self::StorageRanges(m) => list(|p| {
                m.id.encode(p);
                put_list(p, &m.slots, |p, set| {
                    put_list(p, set, |p, s| {
                        list_into(p, |p| {
                            s.hash.encode(p);
                            s.body.encode(p);
                        })
                    })
                });
                put_list(p, &m.proof, |p, n| n.encode(p));
            }),
            Self::GetByteCodes(m) => list(|p| {
                m.id.encode(p);
                put_list(p, &m.hashes, |p, h| h.encode(p));
                m.bytes.encode(p);
            }),
            Self::ByteCodes(m) => list(|p| {
                m.id.encode(p);
                put_list(p, &m.codes, |p, c| c.encode(p));
            }),
            Self::GetTrieNodes(m) => list(|p| {
                m.id.encode(p);
                m.root.encode(p);
                put_list(p, &m.paths, |p, set| put_list(p, set, |p, s| s.encode(p)));
                m.bytes.encode(p);
            }),
            Self::TrieNodes(m) => list(|p| {
                m.id.encode(p);
                put_list(p, &m.nodes, |p, n| n.encode(p));
            }),
        }
    }

    /// Strict inverse of [`Message::encode`]. `code` is snap-relative.
    pub fn decode(code: u64, body: &[u8]) -> Result<Self, CodecError> {
        if body.len() > MAX_MESSAGE {
            return Err(CodecError::TooLarge(body.len()));
        }
        let mut top = body;
        let mut r = Rd::open(&mut top)?;
        if !top.is_empty() {
            return Err(CodecError::Trailing);
        }
        let id = r.u64()?;
        let msg = match code {
            GET_ACCOUNT_RANGE => Self::GetAccountRange(GetAccountRange {
                id,
                root: r.hash()?,
                origin: r.hash()?,
                limit: r.hash()?,
                bytes: r.u64()?,
            }),
            ACCOUNT_RANGE => Self::AccountRange(AccountRange {
                id,
                accounts: r.items("accounts", MAX_ITEMS, |r| {
                    let mut a = r.list()?;
                    let item = AccountData {
                        hash: a.hash()?,
                        body: Bytes::copy_from_slice(a.raw()?),
                    };
                    a.end()?;
                    Ok(item)
                })?,
                proof: r.items("proof", MAX_ITEMS, Rd::bytes_owned)?,
            }),
            GET_STORAGE_RANGES => Self::GetStorageRanges(GetStorageRanges {
                id,
                root: r.hash()?,
                accounts: r.items("accounts", MAX_REQUEST_ITEMS, Rd::hash)?,
                origin: r.bound("origin")?,
                limit: r.bound("limit")?,
                bytes: r.u64()?,
            }),
            STORAGE_RANGES => Self::StorageRanges(StorageRanges {
                id,
                slots: r.items("slot sets", MAX_ITEMS, |r| {
                    r.list()?.rest("slots", MAX_ITEMS, |r| {
                        let mut s = r.list()?;
                        let item = StorageData {
                            hash: s.hash()?,
                            body: s.bytes_owned()?,
                        };
                        s.end()?;
                        Ok(item)
                    })
                })?,
                proof: r.items("proof", MAX_ITEMS, Rd::bytes_owned)?,
            }),
            GET_BYTE_CODES => Self::GetByteCodes(GetByteCodes {
                id,
                hashes: r.items("hashes", MAX_REQUEST_ITEMS, Rd::hash)?,
                bytes: r.u64()?,
            }),
            BYTE_CODES => Self::ByteCodes(ByteCodes {
                id,
                codes: r.items("codes", MAX_ITEMS, Rd::bytes_owned)?,
            }),
            GET_TRIE_NODES => Self::GetTrieNodes(GetTrieNodes {
                id,
                root: r.hash()?,
                paths: r.items("path sets", MAX_REQUEST_ITEMS, |r| {
                    r.list()?.rest("paths", MAX_REQUEST_ITEMS, Rd::bytes_owned)
                })?,
                bytes: r.u64()?,
            }),
            TRIE_NODES => Self::TrieNodes(TrieNodes {
                id,
                nodes: r.items("nodes", MAX_ITEMS, Rd::bytes_owned)?,
            }),
            c => return Err(CodecError::UnknownCode(c)),
        };
        r.end()?;
        Ok(msg)
    }
}

fn list_into(out: &mut Vec<u8>, f: impl FnOnce(&mut Vec<u8>)) {
    let mut p = Vec::new();
    f(&mut p);
    Header {
        list: true,
        payload_length: p.len(),
    }
    .encode(out);
    out.extend_from_slice(&p);
}

fn list(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    list_into(&mut out, f);
    out
}

fn put_list<T>(out: &mut Vec<u8>, items: &[T], f: impl Fn(&mut Vec<u8>, &T)) {
    list_into(out, |p| items.iter().for_each(|i| f(p, i)));
}

/// A cursor over the payload of one RLP list.
struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    /// The payload of the list at the front of `buf`; `buf` moves past it.
    fn open(buf: &mut &'a [u8]) -> Result<Self, CodecError> {
        let h = Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString.into());
        }
        let (payload, rest) = buf
            .split_at_checked(h.payload_length)
            .ok_or(alloy_rlp::Error::InputTooShort)?;
        *buf = rest;
        Ok(Self(payload))
    }

    fn end(&self) -> Result<(), CodecError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(CodecError::Trailing)
        }
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(<u64 as alloy_rlp::Decodable>::decode(&mut self.0)?)
    }

    fn bytes(&mut self) -> Result<&'a [u8], CodecError> {
        Ok(Header::decode_bytes(&mut self.0, false)?)
    }

    fn bytes_owned(&mut self) -> Result<Bytes, CodecError> {
        Ok(Bytes::copy_from_slice(self.bytes()?))
    }

    fn hash(&mut self) -> Result<B256, CodecError> {
        let b = self.bytes()?;
        if b.len() == 32 {
            Ok(B256::from_slice(b))
        } else {
            Err(alloy_rlp::Error::UnexpectedLength.into())
        }
    }

    fn bound(&mut self, what: &'static str) -> Result<Bytes, CodecError> {
        let b = self.bytes()?;
        if b.is_empty() || b.len() == 32 {
            Ok(Bytes::copy_from_slice(b))
        } else {
            Err(CodecError::BadBound(what))
        }
    }

    /// One whole RLP item (string or list) as received, checked for canonical form.
    fn raw(&mut self) -> Result<&'a [u8], CodecError> {
        let (item, rest) = split_item(self.0)?;
        check_item(item, 0)?;
        self.0 = rest;
        Ok(item)
    }

    /// A list of at most `max` items, each read by `f`.
    fn items<T>(
        &mut self,
        what: &'static str,
        max: usize,
        f: impl FnMut(&mut Rd<'a>) -> Result<T, CodecError>,
    ) -> Result<Vec<T>, CodecError> {
        self.list()?.rest(what, max, f)
    }

    /// What is left of this payload as at most `max` items, each read by `f`.
    fn rest<T>(
        &mut self,
        what: &'static str,
        max: usize,
        mut f: impl FnMut(&mut Rd<'a>) -> Result<T, CodecError>,
    ) -> Result<Vec<T>, CodecError> {
        let mut out = Vec::new();
        while !self.0.is_empty() {
            if out.len() >= max {
                return Err(CodecError::TooMany {
                    what,
                    len: out.len().saturating_add(1),
                    max,
                });
            }
            out.push(f(self)?);
        }
        Ok(out)
    }

    /// The list at the front of this payload.
    fn list(&mut self) -> Result<Rd<'a>, CodecError> {
        Rd::open(&mut self.0)
    }
}

fn split_item(buf: &[u8]) -> Result<(&[u8], &[u8]), CodecError> {
    let mut probe = buf;
    let h = Header::decode(&mut probe)?;
    let total = (buf.len() - probe.len())
        .checked_add(h.payload_length)
        .ok_or(alloy_rlp::Error::Overflow)?;
    Ok(buf
        .split_at_checked(total)
        .ok_or(alloy_rlp::Error::InputTooShort)?)
}

fn check_item(item: &[u8], depth: usize) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err(alloy_rlp::Error::Custom("rlp nesting too deep").into());
    }
    let mut b = item;
    let h = Header::decode(&mut b)?;
    if h.list {
        while !b.is_empty() {
            let (one, rest) = split_item(b)?;
            check_item(one, depth + 1)?;
            b = rest;
        }
    }
    Ok(())
}
