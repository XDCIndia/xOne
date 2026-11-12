//! Body checks of geth-XDC `core/block_validator.go` `ValidateBody` and the
//! receipt part of `ValidateState`. Every commitment is recomputed from the
//! body and the receipts; none is taken from the input.
#![allow(
    clippy::as_conversions,
    reason = "signing-tx cap and address-count arithmetic mirror geth int conversions; operands are bounded by the block size; tracked under #9"
)]

use alloy_primitives::{keccak256, logs_bloom, Address, Bloom, Bytes, B256};
use alloy_rlp::{Decodable, Encodable, Header as RlpHeader, RlpDecodable, RlpEncodable};
use alloy_trie::root::ordered_trie_root_encoded;
use xone_chainspec::ChainSpec;
use xone_primitives::Header;

use super::error::BodyError;

/// geth `params.MaxBlockSize` (`params/protocol_params.go:227`).
const MAX_BLOCK_SIZE: usize = 8_388_608;
/// geth `params.BlobTxBlobGasPerBlob`.
const BLOB_GAS_PER_BLOB: u64 = 1 << 17;
/// geth `common.BlockSignersBinary` (`common/xdc_contracts.go:19`).
const BLOCK_SIGNERS: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x89,
]);

/// The block body as it travels: consensus-encoded transactions (legacy: the RLP list; typed:
/// type byte and payload), the uncle headers and, after Shanghai, the encoded withdrawals.
#[derive(Debug, Clone, Default)]
pub struct Body {
    pub transactions: Vec<Bytes>,
    pub uncles: Vec<Header>,
    pub withdrawals: Option<Vec<Bytes>>,
}

#[derive(RlpEncodable, RlpDecodable)]
struct ReceiptFields {
    post_state_or_status: Bytes,
    cumulative_gas_used: u64,
    bloom: Bloom,
    logs: Vec<alloy_primitives::Log>,
}

/// A receipt in geth's consensus encoding (`core/types/receipt.go:91`, `:123`): the same list for
/// every type, with the type byte in front of it for a typed receipt. XDC adds no field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub tx_type: u8,
    pub post_state_or_status: Bytes,
    pub cumulative_gas_used: u64,
    pub logs: Vec<alloy_primitives::Log>,
}

impl Receipt {
    /// Decode `Receipt.MarshalBinary` output. Anything the encoder would not write is rejected.
    pub fn decode(raw: &[u8]) -> Result<Self, BodyError> {
        let bad = |e: alloy_rlp::Error| BodyError::ReceiptDecode(e.to_string());
        let (tx_type, mut rlp) = match raw.first() {
            Some(0xc0..) => (0, raw),
            Some(t @ 1..=4) => (*t, &raw[1..]),
            _ => return Err(BodyError::ReceiptDecode("unsupported receipt type".into())),
        };
        let f = ReceiptFields::decode(&mut rlp).map_err(bad)?;
        if !rlp.is_empty() {
            return Err(BodyError::ReceiptDecode("trailing bytes".into()));
        }
        Ok(Self {
            tx_type,
            post_state_or_status: f.post_state_or_status,
            cumulative_gas_used: f.cumulative_gas_used,
            logs: f.logs,
        })
    }

    pub fn bloom(&self) -> Bloom {
        logs_bloom(self.logs.iter())
    }

    /// `Receipt.MarshalBinary`, with the bloom derived from the logs.
    pub fn encode_consensus(&self) -> Vec<u8> {
        let mut out = if self.tx_type == 0 {
            vec![]
        } else {
            vec![self.tx_type]
        };
        ReceiptFields {
            post_state_or_status: self.post_state_or_status.clone(),
            cumulative_gas_used: self.cumulative_gas_used,
            bloom: self.bloom(),
            logs: self.logs.clone(),
        }
        .encode(&mut out);
        out
    }
}

/// What the body check needs of one transaction.
struct TxFacts {
    to: Option<Address>,
    blobs: u64,
}

fn rlp_items(mut payload: &[u8]) -> Result<Vec<&[u8]>, alloy_rlp::Error> {
    let mut out = vec![];
    while !payload.is_empty() {
        let mut probe = payload;
        let h = RlpHeader::decode(&mut probe)?;
        let end = payload.len() - probe.len() + h.payload_length;
        out.push(payload.get(..end).ok_or(alloy_rlp::Error::InputTooShort)?);
        payload = &payload[end..];
    }
    Ok(out)
}

/// The recipient of a consensus-encoded transaction, `None` for a contract creation.
pub fn tx_recipient(raw: &[u8]) -> Result<Option<Address>, String> {
    tx_facts(raw).map(|f| f.to)
}

/// Field positions per transaction type: (index of `to`, index of the blob hash list).
fn tx_facts(raw: &[u8]) -> Result<TxFacts, String> {
    let (ty, mut rlp) = match raw.first() {
        Some(0xc0..) => (0u8, raw),
        Some(t @ 1..=4) => (*t, &raw[1..]),
        _ => return Err("unsupported transaction type".into()),
    };
    let e = |e: alloy_rlp::Error| e.to_string();
    let list = RlpHeader::decode(&mut rlp).map_err(e)?;
    if !list.list || list.payload_length != rlp.len() {
        return Err("not a single RLP list".into());
    }
    let items = rlp_items(rlp).map_err(e)?;
    let (to_at, blobs_at) = match ty {
        0 => (3, None),
        1 => (4, None),
        2 | 4 => (5, None),
        _ => (5, Some(10)),
    };
    let mut to_item = *items.get(to_at).ok_or("too few fields")?;
    let to = Bytes::decode(&mut to_item).map_err(e)?;
    let to = match to.len() {
        0 => None,
        20 => Some(Address::from_slice(&to)),
        _ => return Err("bad recipient".into()),
    };
    let blobs = match blobs_at {
        Some(i) => {
            let mut hashes = *items.get(i).ok_or("too few fields")?;
            let h = RlpHeader::decode(&mut hashes).map_err(e)?;
            (h.list).then_some(()).ok_or("blob hashes are not a list")?;
            (hashes.len() / 33) as u64
        }
        None => 0,
    };
    Ok(TxFacts { to, blobs })
}

fn list_len(payload: usize) -> usize {
    payload + alloy_rlp::length_of_length(payload)
}

pub struct BodyVerifier<'a> {
    spec: &'a ChainSpec,
}

impl<'a> BodyVerifier<'a> {
    pub fn new(spec: &'a ChainSpec) -> Self {
        Self { spec }
    }

    /// RLP length of the block (geth `Block.Size`): header, transaction list, uncle list and,
    /// when present, the withdrawal list.
    fn block_size(header: &Header, body: &Body) -> usize {
        let txs: usize = body
            .transactions
            .iter()
            .map(|t| {
                if t.first().is_some_and(|b| *b >= 0xc0) {
                    t.len()
                } else {
                    list_len_str(t.len())
                }
            })
            .sum();
        let uncles: usize = body.uncles.iter().map(Encodable::length).sum();
        let mut payload = header.length() + list_len(txs) + list_len(uncles);
        if let Some(w) = body.withdrawals.as_ref().filter(|w| !w.is_empty()) {
            payload += list_len(w.iter().map(|b| b.len()).sum());
        }
        list_len(payload)
    }

    /// geth `ValidateBody` followed by the receipt checks of `ValidateState`: [`Self::verify_body`]
    /// then [`Self::verify_receipts`].
    pub fn verify(
        &self,
        header: &Header,
        body: &Body,
        receipts: &[Bytes],
    ) -> Result<(), BodyError> {
        self.verify_body(header, body)?;
        self.verify_receipts(header, body.transactions.len(), receipts)
    }

    /// geth `ValidateBody` (`core/block_validator.go:52-190`): what the body must satisfy before the
    /// block is executed. Needs no receipts, so an importer runs it first and never executes a body
    /// that does not match its header.
    pub fn verify_body(&self, header: &Header, body: &Body) -> Result<(), BodyError> {
        let (n, spec) = (header.number, self.spec);
        let size = Self::block_size(header, body);
        if spec.osaka.is_active(n) && size > MAX_BLOCK_SIZE {
            return Err(BodyError::BlockOversized(size));
        }
        let facts = body
            .transactions
            .iter()
            .map(|t| tx_facts(t))
            .collect::<Result<Vec<_>, _>>()
            .map_err(BodyError::TxDecode)?;
        // CertiK XDC-04 / XDC-09 (`block_validator.go:292`).
        if spec.signing_tx_bound.is_active(n) {
            if size > MAX_BLOCK_SIZE {
                return Err(BodyError::BlockOversized(size));
            }
            let max = 2 * spec.xdpos.max_masternodes_v2;
            let count = facts
                .iter()
                .filter(|f| f.to == Some(BLOCK_SIGNERS) && spec.effective.tip_signing.is_active(n))
                .count();
            if count as u64 > max {
                return Err(BodyError::TooManySigningTxs { count, max });
            }
        }
        if !body.uncles.is_empty() {
            return Err(BodyError::UnclesNotAllowed);
        }
        let computed = keccak256([alloy_rlp::EMPTY_LIST_CODE]);
        if header.uncle_hash != computed {
            return Err(BodyError::UncleHashMismatch {
                header: header.uncle_hash,
                computed,
            });
        }
        let computed = ordered_trie_root_encoded(&body.transactions);
        if header.tx_root != computed {
            return Err(BodyError::TxRootMismatch {
                header: header.tx_root,
                computed,
            });
        }
        match (&header.withdrawals_hash, &body.withdrawals) {
            (Some(_), None) => return Err(BodyError::WithdrawalsMissing),
            (Some(h), Some(w)) => {
                let computed = ordered_trie_root_encoded(w);
                if *h != computed {
                    return Err(BodyError::WithdrawalsRootMismatch {
                        header: *h,
                        computed,
                    });
                }
            }
            (None, Some(_)) => return Err(BodyError::WithdrawalsUnexpected),
            (None, None) => {}
        }
        let blobs: u64 = facts.iter().map(|f| f.blobs).sum();
        match header.blob_gas_used {
            Some(g) if g / BLOB_GAS_PER_BLOB != blobs => {
                return Err(BodyError::BlobGasMismatch { header: g, blobs })
            }
            None if blobs > 0 => return Err(BodyError::BlobsUnexpected),
            _ => {}
        }
        match (spec.amsterdam.is_active(n), header.block_access_list_hash) {
            (true, None) => return Err(BodyError::AccessListHashMissing),
            (false, Some(_)) => return Err(BodyError::AccessListHashUnexpected),
            _ => {}
        }

        Ok(())
    }

    /// The receipt checks of geth `ValidateState` (`core/block_validator.go:217`, `:227`): count,
    /// bloom and receipt root, from receipts that execution produced. `txs` is the number of
    /// transactions of the body.
    pub fn verify_receipts(
        &self,
        header: &Header,
        txs: usize,
        receipts: &[Bytes],
    ) -> Result<(), BodyError> {
        // Receipts: derived fields only; the bloom comes from the logs.
        if receipts.len() != txs {
            return Err(BodyError::ReceiptCount {
                receipts: receipts.len(),
                txs,
            });
        }
        let decoded = receipts
            .iter()
            .map(|r| Receipt::decode(r))
            .collect::<Result<Vec<_>, _>>()?;
        let bloom = decoded.iter().fold(Bloom::ZERO, |acc, r| acc | r.bloom());
        if bloom != header.bloom {
            return Err(BodyError::BloomMismatch {
                header: header.bloom.to_string(),
                computed: bloom.to_string(),
            });
        }
        let encoded: Vec<Vec<u8>> = decoded.iter().map(Receipt::encode_consensus).collect();
        let computed: B256 = ordered_trie_root_encoded(&encoded);
        if header.receipt_root != computed {
            return Err(BodyError::ReceiptRootMismatch {
                header: header.receipt_root,
                computed,
            });
        }
        Ok(())
    }
}

/// Length of a byte string of `n` bytes (more than one byte, so it has a header).
fn list_len_str(n: usize) -> usize {
    n + alloy_rlp::length_of_length(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty encoded transaction (wire `0x80`) is a typed rejection, not a panic in sizing.
    #[test]
    fn empty_transaction_is_rejected_not_panicking() {
        let spec = xone_chainspec::mainnet();
        let body = Body {
            transactions: vec![Bytes::new()],
            uncles: vec![],
            withdrawals: None,
        };
        let r = BodyVerifier::new(&spec).verify_body(&Header::default(), &body);
        assert!(matches!(r, Err(BodyError::TxDecode(_))), "{r:?}");
    }
}
