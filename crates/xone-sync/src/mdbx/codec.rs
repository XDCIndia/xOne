//! Byte formats of the rows the store keeps in MDBX and in the segments. Segment payloads are
//! RLP (header, body, receipts list), as the ADR says; the rest are fixed layouts. Every decoder
//! returns `None` on a short or malformed input and never panics: the bytes come off a disk.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{Decodable, RlpDecodable, RlpEncodable};
use xone_consensus::{v2::commit::Link, verify::Body};
use xone_primitives::Header;
use xone_state::{Account, StateDiff};

use crate::store::{Block, Record};

#[derive(RlpEncodable, RlpDecodable)]
#[rlp(trailing)]
struct BodyFrame {
    txs: Vec<Bytes>,
    uncles: Vec<Header>,
    withdrawals: Option<Vec<Bytes>>,
}

/// The block RLP of the journal: header, transactions, uncles.
#[derive(RlpEncodable)]
struct BlockFrame<'a> {
    header: &'a Header,
    txs: &'a Vec<Bytes>,
    uncles: &'a Vec<Header>,
}

pub fn header_rlp(h: &Header) -> Vec<u8> {
    alloy_rlp::encode(h)
}

pub fn body_rlp(b: &Body) -> Vec<u8> {
    alloy_rlp::encode(BodyFrame {
        txs: b.transactions.clone(),
        uncles: b.uncles.clone(),
        withdrawals: b.withdrawals.clone(),
    })
}

pub fn receipts_rlp(r: &[Bytes]) -> Vec<u8> {
    let mut v = Vec::new();
    alloy_rlp::encode_list::<Bytes, Bytes>(r, &mut v);
    v
}

pub fn block_rlp(b: &Block) -> Vec<u8> {
    alloy_rlp::encode(BlockFrame {
        header: &b.header,
        txs: &b.body.transactions,
        uncles: &b.body.uncles,
    })
}

pub fn dec_header(b: &[u8]) -> Option<Header> {
    Header::decode(&mut &b[..]).ok()
}

pub fn dec_body(b: &[u8]) -> Option<Body> {
    let f = BodyFrame::decode(&mut &b[..]).ok()?;
    Some(Body {
        transactions: f.txs,
        uncles: f.uncles,
        withdrawals: f.withdrawals,
    })
}

pub fn dec_receipts(b: &[u8]) -> Option<Vec<Bytes>> {
    Vec::<Bytes>::decode(&mut &b[..]).ok()
}

fn put(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(b);
}

/// A cursor over length-prefixed fields.
pub struct Cur<'a>(pub &'a [u8]);

impl<'a> Cur<'a> {
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = b;
        Some(a)
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    pub fn u32(&mut self) -> Option<usize> {
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        usize::try_from(u32::from_le_bytes(b)).ok()
    }
    pub fn field(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()?;
        self.take(n)
    }
    pub fn addr(&mut self) -> Option<Address> {
        Some(Address::from_slice(self.take(20)?))
    }
    pub fn b256(&mut self) -> Option<B256> {
        Some(B256::from_slice(self.take(32)?))
    }
}

/// `n accounts: address, 0 | 1 account-rlp` then `n slots: address, slot, 32 bytes`.
pub fn enc_diff(d: &StateDiff) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(
        &u32::try_from(d.accounts.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for (a, acc) in &d.accounts {
        v.extend_from_slice(a.as_slice());
        match acc {
            None => v.push(0),
            Some(x) => {
                v.push(1);
                put(&mut v, &x.rlp());
            }
        }
    }
    v.extend_from_slice(
        &u32::try_from(d.storage.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for ((a, s), x) in &d.storage {
        v.extend_from_slice(a.as_slice());
        v.extend_from_slice(s.as_slice());
        v.extend_from_slice(&x.to_be_bytes::<32>());
    }
    v
}

pub fn dec_diff(b: &[u8]) -> Option<StateDiff> {
    let mut c = Cur(b);
    let mut d = StateDiff::default();
    for _ in 0..c.u32()? {
        let a = c.addr()?;
        let acc = match c.u8()? {
            0 => None,
            _ => Some(dec_account(c.field()?)?),
        };
        d.accounts.insert(a, acc);
    }
    for _ in 0..c.u32()? {
        let (a, s) = (c.addr()?, c.b256()?);
        d.storage.insert((a, s), U256::from_be_slice(c.take(32)?));
    }
    c.0.is_empty().then_some(d)
}

pub fn dec_account(b: &[u8]) -> Option<Account> {
    Account::decode(&mut &b[..]).ok()
}

/// A record as a side block keeps it: header, body, receipts, diff, aux.
pub fn enc_record(r: &Record) -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, &header_rlp(&r.block.header));
    put(&mut v, &body_rlp(&r.block.body));
    put(&mut v, &receipts_rlp(&r.receipts));
    put(&mut v, &enc_diff(&r.diff));
    put(&mut v, &r.aux);
    v
}

pub fn dec_record(b: &[u8]) -> Option<Record> {
    let mut c = Cur(b);
    let header = dec_header(c.field()?)?;
    let body = dec_body(c.field()?)?;
    let receipts = dec_receipts(c.field()?)?;
    let diff = dec_diff(c.field()?)?;
    let aux = Bytes::copy_from_slice(c.field()?);
    c.0.is_empty().then(|| Record {
        block: Block::new(header, body),
        receipts,
        diff,
        aux,
    })
}

/// A block stored without state.
pub fn enc_block(b: &Block) -> Vec<u8> {
    let mut v = Vec::new();
    put(&mut v, &header_rlp(&b.header));
    put(&mut v, &body_rlp(&b.body));
    v
}

pub fn dec_block(b: &[u8]) -> Option<Block> {
    let mut c = Cur(b);
    let header = dec_header(c.field()?)?;
    let body = dec_body(c.field()?)?;
    c.0.is_empty().then(|| Block::new(header, body))
}

pub fn enc_link(l: &Link) -> Vec<u8> {
    let mut v = l.number.to_le_bytes().to_vec();
    v.extend_from_slice(&l.round.to_le_bytes());
    v.extend_from_slice(l.hash.as_slice());
    v.extend_from_slice(l.parent.as_slice());
    v
}

pub fn dec_link(b: &[u8]) -> Option<Link> {
    let mut c = Cur(b);
    let number = u64::from_le_bytes(c.take(8)?.try_into().ok()?);
    let round = u64::from_le_bytes(c.take(8)?.try_into().ok()?);
    let (hash, parent) = (c.b256()?, c.b256()?);
    c.0.is_empty().then_some(Link {
        number,
        round,
        hash,
        parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_roundtrip_and_short_input() {
        let mut d = StateDiff::default();
        d.accounts.insert(Address::repeat_byte(1), None);
        d.accounts.insert(
            Address::repeat_byte(2),
            Some(Account {
                nonce: 7,
                ..Default::default()
            }),
        );
        d.storage.insert(
            (Address::repeat_byte(2), B256::repeat_byte(3)),
            U256::from(9),
        );
        let b = enc_diff(&d);
        assert_eq!(dec_diff(&b), Some(d));
        for n in 0..b.len() {
            assert!(dec_diff(&b[..n]).is_none(), "cut at {n}");
        }
    }

    #[test]
    fn link_roundtrip() {
        let l = Link {
            number: 4,
            round: 9,
            hash: B256::repeat_byte(1),
            parent: B256::repeat_byte(2),
        };
        assert_eq!(dec_link(&enc_link(&l)), Some(l));
        assert!(dec_link(&enc_link(&l)[1..]).is_none());
    }
}
