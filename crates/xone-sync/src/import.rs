//! Offline import: a geth block file feeds the staged pipeline through [`FileSource`]. The file is
//! the `exportChain` format of geth (`cmd/utils/cmd.go:479-499`, `core/types/block.go:278-283`):
//! the RLP of each block back to back, `[header, [transactions], [uncles]]`, ascending, no
//! framing. Block 0 is skipped (`cmd/utils/cmd.go:245-249`) and blocks the chain already holds
//! are skipped (`missingBlocks`, `:377-393`). The batch is the staged window, not geth's 2,500.
//! A `.gz` file is not read (geth wraps it in gzip, `:213-222`). Spec: `spec/sync/import-export.md`.

use crate::stages::{Source, SourceError};
use alloy_primitives::Bytes;
use alloy_rlp::{Decodable, Header as RlpHeader};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
};
use xone_consensus::verify::Body;
use xone_primitives::Header;

/// A block of the file is larger than any block the network can carry.
const MAX_BLOCK: u64 = 64 << 20;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("block of {0} bytes exceeds the cap")]
    TooLarge(u64),
    #[error("withdrawals or an access list follow the uncles: not an XDC block")]
    Trailing,
}

/// `[header, [transactions], [uncles]]`. A typed transaction is an RLP string whose content is the
/// consensus encoding, a legacy one is the list itself (the shape `Body::transactions` keeps).
pub fn decode_block(raw: &[u8]) -> Result<(Header, Body), FileError> {
    let mut buf = raw;
    let outer = RlpHeader::decode(&mut buf)?;
    if !outer.list {
        return Err(alloy_rlp::Error::UnexpectedString.into());
    }
    if buf.len() != outer.payload_length {
        return Err(FileError::Trailing);
    }
    let header = Header::decode(&mut buf)?;
    let list = RlpHeader::decode(&mut buf)?;
    if !list.list {
        return Err(alloy_rlp::Error::UnexpectedString.into());
    }
    let (mut txs, mut rest) = buf
        .split_at_checked(list.payload_length)
        .ok_or(alloy_rlp::Error::InputTooShort)?;
    let mut transactions = vec![];
    while !txs.is_empty() {
        let start = txs;
        let item = RlpHeader::decode(&mut txs)?;
        let head_len = start.len() - txs.len();
        let (item_bytes, after) = txs
            .split_at_checked(item.payload_length)
            .ok_or(alloy_rlp::Error::InputTooShort)?;
        transactions.push(if item.list {
            Bytes::copy_from_slice(&start[..head_len + item.payload_length])
        } else {
            Bytes::copy_from_slice(item_bytes)
        });
        txs = after;
    }
    let uncles = Vec::<Header>::decode(&mut rest)?;
    if !rest.is_empty() {
        return Err(FileError::Trailing);
    }
    Ok((
        header,
        Body {
            transactions,
            uncles,
            withdrawals: None,
        },
    ))
}

/// A seekable block file as a pipeline [`Source`]. Headers are read forward in windows; the bodies
/// of a window are read again by offset, so no body is held while headers are staged. After a restart
/// the offsets are unknown and the file is scanned once from the start by item lengths.
pub struct FileSource<R> {
    r: R,
    /// Number of the first block of the file.
    first: Option<u64>,
    /// Number and offset of the next unread block.
    cursor: (u64, u64),
    /// Offsets of the blocks of the last header window.
    window: BTreeMap<u64, u64>,
}

impl<R: Read + Seek> FileSource<R> {
    pub fn new(r: R) -> Self {
        Self {
            r,
            first: None,
            cursor: (0, 0),
            window: BTreeMap::new(),
        }
    }

    /// Length of the item at `at`: header bytes and payload bytes. `None` at the end of the file.
    fn item_len(&mut self, at: u64) -> Result<Option<(u64, u64)>, FileError> {
        self.r.seek(SeekFrom::Start(at))?;
        let mut p = [0u8; 1];
        if self.r.read(&mut p)? == 0 {
            return Ok(None);
        }
        match p[0] {
            0xc0..=0xf7 => Ok(Some((1, u64::from(p[0] - 0xc0)))),
            0xf8.. => {
                let n = usize::from(p[0] - 0xf7);
                let mut b = [0u8; 8];
                self.r.read_exact(&mut b[8 - n..])?;
                let len = u64::from_be_bytes(b);
                // Canonical form: no leading zero byte and a length that needs the long form.
                if b[8 - n] == 0 || len < 56 {
                    return Err(alloy_rlp::Error::LeadingZero.into());
                }
                let h = 1 + u64::from(p[0] - 0xf7);
                at.checked_add(h)
                    .and_then(|x| x.checked_add(len))
                    .ok_or(FileError::TooLarge(u64::MAX))?;
                Ok(Some((h, len)))
            }
            _ => Err(alloy_rlp::Error::UnexpectedString.into()),
        }
    }

    fn read_at(&mut self, at: u64) -> Result<Option<(Vec<u8>, u64)>, FileError> {
        let Some((h, p)) = self.item_len(at)? else {
            return Ok(None);
        };
        if h + p > MAX_BLOCK {
            return Err(FileError::TooLarge(h + p));
        }
        self.r.seek(SeekFrom::Start(at))?;
        let mut v = vec![0; usize::try_from(h + p).map_err(|_| FileError::TooLarge(h + p))?];
        self.r.read_exact(&mut v)?;
        Ok(Some((v, at + h + p)))
    }

    fn first_number(&mut self) -> Result<Option<u64>, FileError> {
        if self.first.is_none() {
            let Some((raw, _)) = self.read_at(0)? else {
                return Ok(None);
            };
            let n = decode_block(&raw)?.0.number;
            self.first = Some(n);
            self.cursor = (n, 0);
        }
        Ok(self.first)
    }

    /// The offset of block `number`, by skipping items from the nearest known position.
    fn offset_of(&mut self, number: u64) -> Result<Option<u64>, FileError> {
        if let Some(o) = self.window.get(&number) {
            return Ok(Some(*o));
        }
        let Some(first) = self.first_number()? else {
            return Ok(None);
        };
        if number < first {
            return Ok(None);
        }
        let (mut n, mut at) = if number >= self.cursor.0 {
            self.cursor
        } else {
            (first, 0)
        };
        while n < number {
            let Some((h, p)) = self.item_len(at)? else {
                return Ok(None);
            };
            (n, at) = (n + 1, at + h + p);
        }
        Ok(Some(at))
    }
}

fn err(e: FileError) -> SourceError {
    SourceError(e.to_string())
}

impl<R: Read + Seek> Source for FileSource<R> {
    fn headers(&mut self, from: u64, max: usize) -> Result<Vec<Header>, SourceError> {
        let from = from.max(1); // block 0 is never imported
        let Some(mut at) = self.offset_of(from).map_err(err)? else {
            return match self.first_number().map_err(err)? {
                Some(f) if f > from => Err(SourceError(format!(
                    "the file starts at block {f}, the chain needs block {from}"
                ))),
                _ => Ok(vec![]),
            };
        };
        self.window.clear();
        let mut out = Vec::new();
        while out.len() < max {
            let Some((raw, next)) = self.read_at(at).map_err(err)? else {
                break;
            };
            let header = decode_block(&raw).map_err(err)?.0;
            self.window.insert(header.number, at);
            at = next;
            self.cursor = (header.number + 1, at);
            out.push(header);
        }
        Ok(out)
    }

    fn bodies(&mut self, headers: &[Header], max_bytes: usize) -> Result<Vec<Body>, SourceError> {
        let (mut out, mut bytes) = (Vec::with_capacity(headers.len()), 0usize);
        for h in headers {
            if !out.is_empty() && bytes > max_bytes {
                break;
            }
            let Some(at) = self.offset_of(h.number).map_err(err)? else {
                break;
            };
            let Some((raw, _)) = self.read_at(at).map_err(err)? else {
                break;
            };
            let (header, body) = decode_block(&raw).map_err(err)?;
            if header.hash() != h.hash() {
                return Err(SourceError(format!(
                    "block {} of the file is not the staged header",
                    h.number
                )));
            }
            bytes += body.transactions.iter().map(|t| t.len()).sum::<usize>();
            out.push(body);
        }
        Ok(out)
    }
}
