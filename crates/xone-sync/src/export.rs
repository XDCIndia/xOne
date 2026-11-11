//! Offline export: the canonical blocks of a [`Store`] as a geth block file, byte for byte what
//! geth's `exportChain` writes (`cmd/utils/cmd.go:479-499`, `core/blockchain.go:1840-1869`): per
//! block the RLP of `[header, [transactions], [uncles]]`, ascending, a check that each block links
//! to the one before. Spec: `spec/sync/import-export.md`.

use crate::store::Store;
use alloy_rlp::Encodable;
use std::io::Write;
use xone_consensus::verify::Body;
use xone_primitives::Header;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("export failed: first ({first}) is greater than last ({last})")]
    Range { first: u64, last: u64 },
    #[error("export failed on #{0}: not found")]
    NotFound(u64),
    #[error("export failed: chain reorg during export")]
    Reorg,
}

/// The inverse of [`crate::import::decode_block`].
pub fn encode_block(header: &Header, body: &Body) -> Vec<u8> {
    let mut txs = Vec::new();
    for t in &body.transactions {
        if t.first().is_some_and(|b| *b >= 0xc0) {
            txs.extend_from_slice(t); // legacy: already the RLP list
        } else {
            t.as_ref().encode(&mut txs); // typed: an RLP string
        }
    }
    let mut payload = alloy_rlp::encode(header);
    alloy_rlp::Header {
        list: true,
        payload_length: txs.len(),
    }
    .encode(&mut payload);
    payload.extend_from_slice(&txs);
    body.uncles.encode(&mut payload);
    let mut out = Vec::with_capacity(payload.len() + 9);
    alloy_rlp::Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

/// Writes blocks `first..=last` of the canonical chain; returns how many.
pub fn export<S: Store>(
    store: &S,
    first: u64,
    last: u64,
    w: &mut impl Write,
) -> Result<u64, ExportError> {
    if first > last {
        return Err(ExportError::Range { first, last });
    }
    let mut parent = None;
    for n in first..=last {
        let hash = store.canonical_hash(n).ok_or(ExportError::NotFound(n))?;
        let rec = store.record(hash).ok_or(ExportError::NotFound(n))?;
        if parent.is_some_and(|p| p != rec.block.parent()) {
            return Err(ExportError::Reorg);
        }
        parent = Some(hash);
        w.write_all(&encode_block(&rec.block.header, &rec.block.body))?;
    }
    Ok(last - first + 1)
}
