//! The snapshot container (`spec/bootstrap/snapshot-format.md`): a magic, the format version, then
//! chunks, then a trailer chunk that names every chunk before it.
//!
//! ```text
//! file  = "XSNP" | version u16 LE | chunk* | trailer
//! chunk = kind u8 | seq u32 LE | raw_len u32 LE | comp_len u32 LE | gzip(raw) | crc32c u32 LE
//! ```
//!
//! The CRC-32C covers the 13 header bytes and the compressed payload. A reader stops at the first
//! chunk that does not verify and says how many chunks were whole and where the next one starts,
//! which is the offset to resume from. Gzip is the one of `flate2` with a zero timestamp, so the
//! same content gives the same bytes. No clock and no global state.

use std::io::{self, Read, Write};

use alloy_primitives::B256;
use alloy_rlp::{RlpDecodable, RlpEncodable};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};

use super::anchor::FORMAT_VERSION;

pub const MAGIC: [u8; 4] = *b"XSNP";
/// Magic and version.
pub const HEAD_LEN: u64 = 6;
/// A chunk is at most this many bytes raw or compressed: a bound on what a damaged length can make
/// the reader allocate.
pub const MAX_CHUNK: u32 = 64 << 20;
const CHUNK_HEAD: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Manifest = 1,
    Accounts = 2,
    Storage = 3,
    Codes = 4,
    Side = 5,
    Headers = 6,
    Bodies = 7,
    Receipts = 8,
    Trailer = 255,
}

impl Kind {
    pub fn code(self) -> u8 {
        match self {
            Self::Manifest => 1,
            Self::Accounts => 2,
            Self::Storage => 3,
            Self::Codes => 4,
            Self::Side => 5,
            Self::Headers => 6,
            Self::Bodies => 7,
            Self::Receipts => 8,
            Self::Trailer => 255,
        }
    }

    fn of(b: u8) -> Option<Self> {
        Some(match b {
            1 => Self::Manifest,
            2 => Self::Accounts,
            3 => Self::Storage,
            4 => Self::Codes,
            5 => Self::Side,
            6 => Self::Headers,
            7 => Self::Bodies,
            8 => Self::Receipts,
            255 => Self::Trailer,
            _ => return None,
        })
    }
}

/// CRC-32C (Castagnoli, reflected, polynomial 0x82F63B78) over `parts`.
/// ponytail: bitwise, 8 steps a byte; gzip costs more per byte. A table or SSE4.2 if it shows.
pub fn crc32c(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for b in parts.iter().flat_map(|p| p.iter()) {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = (c >> 1) ^ (0x82F6_3B78 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}

/// A length as `u64` (a `usize` is never wider).
pub fn len64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// One entry of the trailer: a chunk that precedes it.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct ChunkRef {
    pub kind: u64,
    pub seq: u64,
    pub raw_len: u64,
    pub crc: u64,
}

/// The last chunk: every chunk before it, the hash of the manifest, and the first block held of
/// each range (ADR-005 section 6.3 `first_block`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Trailer {
    /// keccak256 of the manifest chunk's raw bytes (the RLP of the anchor).
    pub manifest_hash: B256,
    pub chunks: Vec<ChunkRef>,
    pub headers_first: u64,
    pub records_first: u64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    #[error("not a snapshot file")]
    BadMagic,
    #[error("snapshot format version {found} is not supported (this build reads {supported})")]
    UnsupportedFormat { found: u16, supported: u16 },
    /// The file ends inside chunk number `chunks`. `offset` is the end of the last whole chunk:
    /// continue the download from there.
    #[error("the file ends after {chunks} whole chunks, at byte {offset}")]
    Truncated { chunks: u32, offset: u64 },
    #[error("chunk {seq} does not match its CRC-32C")]
    BadCrc { seq: u32 },
    #[error("chunk {got} where chunk {want} comes next")]
    BadSeq { want: u32, got: u32 },
    #[error("chunk {seq} has kind {kind}, which is not a chunk kind")]
    UnknownKind { seq: u32, kind: u8 },
    #[error("chunk {seq} is larger than the cap")]
    TooLarge { seq: u32 },
    #[error("chunk {seq} does not inflate to its stated length")]
    BadGzip { seq: u32 },
    #[error("io: {0}")]
    Io(String),
}

fn io_err(e: io::Error) -> FormatError {
    FormatError::Io(e.to_string())
}

pub struct Writer<W: Write> {
    w: W,
    seq: u32,
    refs: Vec<ChunkRef>,
    bytes: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        w.write_all(&MAGIC)?;
        w.write_all(&FORMAT_VERSION.to_le_bytes())?;
        Ok(Self {
            w,
            seq: 0,
            refs: Vec::new(),
            bytes: HEAD_LEN,
        })
    }

    fn put(&mut self, kind: Kind, raw: &[u8]) -> io::Result<ChunkRef> {
        let too_big = || io::Error::new(io::ErrorKind::InvalidInput, "chunk over the cap");
        let mut z = GzEncoder::new(Vec::new(), Compression::fast());
        z.write_all(raw)?;
        let comp = z.finish()?;
        let (raw_len, comp_len) = (
            u32::try_from(raw.len()).map_err(|_| too_big())?,
            u32::try_from(comp.len()).map_err(|_| too_big())?,
        );
        if raw_len > MAX_CHUNK || comp_len > MAX_CHUNK {
            return Err(too_big());
        }
        let mut head = [0u8; CHUNK_HEAD];
        head[0] = kind.code();
        head[1..5].copy_from_slice(&self.seq.to_le_bytes());
        head[5..9].copy_from_slice(&raw_len.to_le_bytes());
        head[9..13].copy_from_slice(&comp_len.to_le_bytes());
        let crc = crc32c(&[&head, &comp]);
        self.w.write_all(&head)?;
        self.w.write_all(&comp)?;
        self.w.write_all(&crc.to_le_bytes())?;
        self.bytes += len64(CHUNK_HEAD + comp.len() + 4);
        let r = ChunkRef {
            kind: u64::from(kind.code()),
            seq: u64::from(self.seq),
            raw_len: u64::from(raw_len),
            crc: u64::from(crc),
        };
        self.seq += 1;
        Ok(r)
    }

    pub fn chunk(&mut self, kind: Kind, raw: &[u8]) -> io::Result<()> {
        let r = self.put(kind, raw)?;
        self.refs.push(r);
        Ok(())
    }

    /// Writes the trailer, which names every chunk written so far; returns the size of the file.
    pub fn finish(
        mut self,
        manifest_hash: B256,
        headers_first: u64,
        records_first: u64,
    ) -> io::Result<u64> {
        let t = Trailer {
            manifest_hash,
            chunks: std::mem::take(&mut self.refs),
            headers_first,
            records_first,
        };
        self.finish_with(&t)
    }

    /// [`Writer::finish`] with a trailer of the caller's making (the tests of a damaged one).
    pub fn finish_with(mut self, t: &Trailer) -> io::Result<u64> {
        self.put(Kind::Trailer, &alloy_rlp::encode(t))?;
        self.w.flush()?;
        Ok(self.bytes)
    }
}

/// A whole chunk, inflated and checked.
pub struct Chunk {
    pub kind: Kind,
    pub seq: u32,
    pub raw: Vec<u8>,
    pub crc: u32,
    /// The chunk as it is in the file, for a staging copy.
    pub wire: Vec<u8>,
}

impl Chunk {
    pub fn reference(&self) -> ChunkRef {
        ChunkRef {
            kind: u64::from(self.kind.code()),
            seq: u64::from(self.seq),
            raw_len: len64(self.raw.len()),
            crc: u64::from(self.crc),
        }
    }
}

pub struct Reader<R: Read> {
    r: R,
    next: u32,
    /// End of the last whole chunk.
    offset: u64,
}

/// Reads the magic and the version. A later version is refused before any chunk is read.
pub fn read_head(r: &mut impl Read) -> Result<(), FormatError> {
    let mut h = [0u8; 6];
    r.read_exact(&mut h).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FormatError::Truncated {
            chunks: 0,
            offset: 0,
        },
        _ => io_err(e),
    })?;
    if h[..4] != MAGIC {
        return Err(FormatError::BadMagic);
    }
    let found = u16::from_le_bytes([h[4], h[5]]);
    if found != FORMAT_VERSION {
        return Err(FormatError::UnsupportedFormat {
            found,
            supported: FORMAT_VERSION,
        });
    }
    Ok(())
}

impl<R: Read> Reader<R> {
    /// `r` is positioned at the start of chunk `next`, which is byte `offset` of the file.
    pub fn at(r: R, next: u32, offset: u64) -> Self {
        Self { r, next, offset }
    }

    pub fn new(mut r: R) -> Result<Self, FormatError> {
        read_head(&mut r)?;
        Ok(Self::at(r, 0, HEAD_LEN))
    }

    /// The end of the last whole chunk and the number of whole chunks.
    pub fn position(&self) -> (u64, u32) {
        (self.offset, self.next)
    }

    fn exact(&mut self, buf: &mut [u8]) -> Result<(), FormatError> {
        self.r.read_exact(buf).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => FormatError::Truncated {
                chunks: self.next,
                offset: self.offset,
            },
            _ => io_err(e),
        })
    }

    /// The next chunk; `None` at a clean end of file.
    pub fn read_chunk(&mut self) -> Result<Option<Chunk>, FormatError> {
        let mut head = [0u8; CHUNK_HEAD];
        let n = self.r.read(&mut head[..1]).map_err(io_err)?;
        if n == 0 {
            return Ok(None);
        }
        self.exact(&mut head[1..])?;
        let seq = self.next;
        let kind = Kind::of(head[0]).ok_or(FormatError::UnknownKind { seq, kind: head[0] })?;
        let got = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
        let raw_len = u32::from_le_bytes([head[5], head[6], head[7], head[8]]);
        let comp_len = u32::from_le_bytes([head[9], head[10], head[11], head[12]]);
        if raw_len > MAX_CHUNK || comp_len > MAX_CHUNK {
            return Err(FormatError::TooLarge { seq });
        }
        let mut comp =
            vec![0u8; usize::try_from(comp_len).map_err(|_| FormatError::TooLarge { seq })?];
        self.exact(&mut comp)?;
        let mut tail = [0u8; 4];
        self.exact(&mut tail)?;
        let crc = u32::from_le_bytes(tail);
        if crc != crc32c(&[&head, &comp]) {
            return Err(FormatError::BadCrc { seq });
        }
        if got != seq {
            return Err(FormatError::BadSeq { want: seq, got });
        }
        let mut raw = Vec::with_capacity(usize::try_from(raw_len).unwrap_or(0));
        let bad = || FormatError::BadGzip { seq };
        GzDecoder::new(&comp[..])
            .take(u64::from(raw_len) + 1)
            .read_to_end(&mut raw)
            .map_err(|_| bad())?;
        if len64(raw.len()) != u64::from(raw_len) {
            return Err(bad());
        }
        self.next += 1;
        let mut wire = head.to_vec();
        wire.extend_from_slice(&comp);
        wire.extend_from_slice(&tail);
        self.offset += len64(wire.len());
        Ok(Some(Chunk {
            kind,
            seq,
            raw,
            crc,
            wire,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_check_value() {
        // RFC 3720 appendix B.4 and the standard check string
        assert_eq!(crc32c(&[b"123456789"]), 0xE306_9283);
        assert_eq!(crc32c(&[b"1234", b"56789"]), 0xE306_9283);
        assert_eq!(crc32c(&[&[0u8; 32]]), 0x8A91_36AA);
    }

    #[test]
    fn roundtrip_and_cut_points() {
        let mut file = Vec::new();
        let mut w = Writer::new(&mut file).unwrap();
        w.chunk(Kind::Manifest, b"a manifest").unwrap();
        w.chunk(Kind::Codes, &[7u8; 5000]).unwrap();
        assert_eq!(
            w.finish(B256::repeat_byte(1), 3, 4).unwrap(),
            len64(file.len())
        );
        let mut r = Reader::new(&file[..]).unwrap();
        let mut seen = vec![];
        while let Some(c) = r.read_chunk().unwrap() {
            seen.push((c.kind, c.raw.len()));
        }
        assert_eq!(
            seen.iter().map(|s| s.0).collect::<Vec<_>>(),
            [Kind::Manifest, Kind::Codes, Kind::Trailer]
        );
        // every cut inside a chunk is Truncated at the end of the last whole chunk
        for cut in 7..file.len() {
            let mut r = Reader::new(&file[..cut]).unwrap();
            let e = loop {
                match r.read_chunk() {
                    Ok(Some(_)) => {}
                    Ok(None) => break None,
                    Err(e) => break Some(e),
                }
            };
            match e {
                Some(FormatError::Truncated { offset, .. }) => assert!(offset <= len64(cut)),
                None => assert!(file.len() > cut && r.position().0 == len64(cut)),
                other => panic!("{other:?} at cut {cut}"),
            }
        }
        // a flipped byte anywhere in a chunk is refused
        for at in 6..file.len() {
            let mut bad = file.clone();
            bad[at] ^= 1;
            let mut r = Reader::new(&bad[..]).unwrap();
            let mut refused = false;
            for _ in 0..4 {
                match r.read_chunk() {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        refused = true;
                        break;
                    }
                }
            }
            assert!(refused, "flip at {at} accepted");
        }
    }
}
