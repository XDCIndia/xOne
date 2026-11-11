//! The block cache: one JSON line per block, `{"n", "raw", "hash"?}`. `raw` is the archive's
//! `debug_getRawBlock` (the block RLP, so transactions keep their consensus encoding); `hash` is
//! the archive's own block hash, kept for every 100th block and the last one fetched. The cache
//! is read plain or gzipped (the committed vectors are gzipped).

use alloy_primitives::{Bytes, B256};
use alloy_rlp::{Decodable, Header as RlpHeader};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::Path,
};
use xone_chainspec::Network;
use xone_consensus::verify::Body;
use xone_primitives::Header;
use xone_sweep::fetch::Rpc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub n: u64,
    pub raw: Bytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<B256>,
}

pub struct Block {
    pub n: u64,
    pub archive_hash: Option<B256>,
    pub header: Header,
    pub body: Body,
}

pub fn read(path: &Path) -> Result<Vec<Line>, String> {
    read_limit(path, usize::MAX)
}

/// The first `limit` lines (a cache of a whole chain is gigabytes: stop reading at the block asked for).
pub fn read_limit(path: &Path, limit: usize) -> Result<Vec<Line>, String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let r: Box<dyn BufRead> = if path.extension().is_some_and(|e| e == "gz") {
        Box::new(BufReader::new(GzDecoder::new(f)))
    } else {
        Box::new(BufReader::new(f))
    };
    r.lines()
        .take(limit)
        .map(|l| serde_json::from_str(&l.map_err(|e| e.to_string())?).map_err(|e| e.to_string()))
        .collect()
}

/// Write `lines` gzipped.
pub fn write_gz(path: &Path, lines: &[Line]) -> Result<(), String> {
    let mut z = GzEncoder::new(
        File::create(path).map_err(|e| e.to_string())?,
        Compression::best(),
    );
    for l in lines {
        writeln!(
            z,
            "{}",
            serde_json::to_string(l).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
    }
    z.finish().map(|_| ()).map_err(|e| e.to_string())
}

/// `[header, [transactions], [uncles]]` into a header and a body. A typed transaction is an RLP
/// string whose content is the consensus encoding; a legacy one is the list itself.
pub fn decode(l: &Line) -> Result<Block, String> {
    let e = |e: alloy_rlp::Error| format!("block {}: {e}", l.n);
    let mut buf: &[u8] = &l.raw;
    let outer = RlpHeader::decode(&mut buf).map_err(e)?;
    let mut buf = buf.get(..outer.payload_length).ok_or("short block")?;
    let header = Header::decode(&mut buf).map_err(e)?;
    let list = RlpHeader::decode(&mut buf).map_err(e)?;
    let (mut txs, rest) = buf.split_at(list.payload_length);
    let mut transactions = vec![];
    while !txs.is_empty() {
        let start = txs;
        let item = RlpHeader::decode(&mut txs).map_err(e)?;
        let consumed = start.len() - txs.len();
        transactions.push(if item.list {
            Bytes::copy_from_slice(&start[..consumed + item.payload_length])
        } else {
            Bytes::copy_from_slice(&txs[..item.payload_length])
        });
        txs = &txs[item.payload_length..];
    }
    let mut rest = rest;
    let uncles = Vec::<Header>::decode(&mut rest).map_err(e)?;
    Ok(Block {
        n: l.n,
        archive_hash: l.hash,
        header,
        body: Body {
            transactions,
            uncles,
            withdrawals: None,
        },
    })
}

/// The number of the last line of the cache, read from its tail (a whole-chain cache is gigabytes).
fn last_block(path: &Path) -> Result<Option<u64>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let tail = size.min(64 << 20);
    f.seek(SeekFrom::Start(size - tail)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    // a line is complete when a newline ends it; the first line of the window may be cut
    let mut lines: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
    lines.pop(); // after the last newline
    let first_cut = size > tail;
    let usable = lines.iter().skip(usize::from(first_cut)).next_back();
    match usable {
        None if size == 0 => Ok(None),
        None => Err("a line of the cache is longer than 64 MB".into()),
        Some(l) => {
            let v: serde_json::Value = serde_json::from_slice(l).map_err(|e| e.to_string())?;
            v["n"].as_u64().map(Some).ok_or("last line has no block number".into())
        }
    }
}

/// Append blocks `from..=to` that the cache at `path` does not hold yet. A cache starts at the
/// block of its first line and must stay contiguous: a new file may start anywhere, an existing
/// one continues after its last line. Batches of `BATCH` blocks, one request every 260 ms, so at
/// most 4 requests per second (`xone_sweep::fetch::Rpc`).
pub fn fetch(net: Network, path: &Path, from: u64, to: u64) -> Result<u64, String> {
    const BATCH: u64 = 200;
    let next = if path.exists() {
        last_block(path)?.map_or(from, |n| n + 1)
    } else {
        from
    };
    let start = from.max(next);
    if start > next {
        return Err(format!(
            "the cache ends before block {next}, cannot start at {start}"
        ));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut rpc = Rpc::for_network(net)?;
    let mut n = start;
    // The archive refuses a response above its size limit and answers the blocks past the limit
    // with an error: write the blocks before the first error and ask again with a smaller batch.
    let mut batch = BATCH;
    while n <= to {
        let last = (n + batch - 1).min(to);
        let mut calls = vec![];
        for k in n..=last {
            calls.push(("debug_getRawBlock", json!([format!("{k:#x}")])));
        }
        let answers = rpc.batch(calls)?;
        let good = answers.iter().take_while(|a| a["error"].is_null()).count() as u64;
        if good == 0 {
            if batch == 1 {
                return Err(format!("block {n}: {}", answers[0]["error"]));
            }
            batch = (batch / 2).max(1);
            continue;
        }
        let last = n + good - 1;
        let hashed: Vec<u64> = (n..=last).filter(|k| k % 100 == 0 || *k == to).collect();
        let hashes = if hashed.is_empty() {
            vec![]
        } else {
            let c = hashed
                .iter()
                .map(|k| ("eth_getBlockByNumber", json!([format!("{k:#x}"), false])))
                .collect();
            rpc.batch(c)?
        };
        for (i, a) in answers.iter().take(good as usize).enumerate() {
            let k = n + i as u64;
            let raw: Bytes = serde_json::from_value(a["result"].clone())
                .map_err(|_| format!("block {k}: {}", a["error"]))?;
            let hash = hashed
                .iter()
                .position(|h| *h == k)
                .map(|j| serde_json::from_value::<B256>(hashes[j]["result"]["hash"].clone()))
                .transpose()
                .map_err(|e| e.to_string())?;
            let line = Line { n: k, raw, hash };
            writeln!(
                out,
                "{}",
                serde_json::to_string(&line).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
        out.flush().map_err(|e| e.to_string())?;
        if good < batch.min(to + 1 - n) {
            batch = (good).max(1);
        } else {
            batch = (batch * 2).min(BATCH);
        }
        let prev = n;
        n = last + 1;
        if n / 10_000 != prev / 10_000 {
            eprintln!("fetched to {}", n - 1);
        }
    }
    Ok(to + 1 - start)
}
