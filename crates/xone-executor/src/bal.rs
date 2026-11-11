//! The EIP-7928 block access list as geth-XDC builds it (`core/types/bal/bal.go`,
//! `bal_encoding.go`) and its hash (`BlockAccessList.Hash`, `bal_encoding.go:107-118`): the accounts
//! sorted by address, each `[address, storage changes, storage reads, balance changes, nonce
//! changes, code changes]`, every change keyed by its block access index (0 the pre-execution
//! system call, `i + 1` transaction `i`, `n + 1` the post-execution phase).

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::RlpEncodable;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Access {
    writes: BTreeMap<B256, BTreeMap<u32, U256>>,
    reads: BTreeSet<B256>,
    balance: BTreeMap<u32, U256>,
    nonce: BTreeMap<u32, u64>,
    code: BTreeMap<u32, Bytes>,
}

#[derive(Default)]
pub struct Bal {
    accounts: BTreeMap<Address, Access>,
}

#[derive(RlpEncodable)]
struct IndexedValue {
    index: u32,
    value: U256,
}
#[derive(RlpEncodable)]
struct IndexedNonce {
    index: u32,
    nonce: u64,
}
#[derive(RlpEncodable)]
struct IndexedCode {
    index: u32,
    code: Bytes,
}
#[derive(RlpEncodable)]
struct SlotChanges {
    slot: U256,
    changes: Vec<IndexedValue>,
}
#[derive(RlpEncodable)]
struct AccountAccess {
    address: Address,
    storage_changes: Vec<SlotChanges>,
    storage_reads: Vec<U256>,
    balance_changes: Vec<IndexedValue>,
    nonce_changes: Vec<IndexedNonce>,
    code_changes: Vec<IndexedCode>,
}

fn word(k: B256) -> U256 {
    U256::from_be_bytes(k.0)
}

impl Bal {
    /// `AccountRead`: the account was loaded, whether or not it exists.
    pub fn read(&mut self, a: Address) {
        self.accounts.entry(a).or_default();
    }

    /// `StorageRead`: a slot written in the block is listed under its writes only.
    pub fn storage_read(&mut self, a: Address, k: B256) {
        let acc = self.accounts.entry(a).or_default();
        if !acc.writes.contains_key(&k) {
            acc.reads.insert(k);
        }
    }

    /// `StorageWrite`: the post-transaction value of a slot that changed.
    pub fn storage_write(&mut self, index: u32, a: Address, k: B256, v: B256) {
        let acc = self.accounts.entry(a).or_default();
        acc.writes.entry(k).or_default().insert(index, word(v));
        acc.reads.remove(&k);
    }

    pub fn balance(&mut self, index: u32, a: Address, v: U256) {
        self.accounts.entry(a).or_default().balance.insert(index, v);
    }

    pub fn nonce(&mut self, index: u32, a: Address, n: u64) {
        self.accounts.entry(a).or_default().nonce.insert(index, n);
    }

    /// `CodeChange`: the code after the transaction.
    pub fn code(&mut self, index: u32, a: Address, code: Bytes) {
        self.accounts.entry(a).or_default().code.insert(index, code);
    }

    /// keccak256 of the RLP of the sorted list.
    pub fn hash(&self) -> B256 {
        let list: Vec<AccountAccess> = self
            .accounts
            .iter()
            .map(|(a, x)| AccountAccess {
                address: *a,
                storage_changes: x
                    .writes
                    .iter()
                    .map(|(k, w)| SlotChanges {
                        slot: word(*k),
                        changes: w
                            .iter()
                            .map(|(i, v)| IndexedValue {
                                index: *i,
                                value: *v,
                            })
                            .collect(),
                    })
                    .collect(),
                storage_reads: x.reads.iter().map(|k| word(*k)).collect(),
                balance_changes: x
                    .balance
                    .iter()
                    .map(|(i, v)| IndexedValue {
                        index: *i,
                        value: *v,
                    })
                    .collect(),
                nonce_changes: x
                    .nonce
                    .iter()
                    .map(|(i, n)| IndexedNonce {
                        index: *i,
                        nonce: *n,
                    })
                    .collect(),
                code_changes: x
                    .code
                    .iter()
                    .map(|(i, c)| IndexedCode {
                        index: *i,
                        code: c.clone(),
                    })
                    .collect(),
            })
            .collect();
        keccak256(alloy_rlp::encode(&list))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};

    /// Block 21,600 of the net2040 archive: no transactions, so the list holds the EIP-2935 write of
    /// the parent hash at index 0 and nothing else; its header carries this hash.
    #[test]
    fn empty_block_hash_equals_the_archive_header() {
        let mut b = Bal::default();
        b.storage_write(
            0,
            address!("0000F90827F1C53a10cb7A02335B175320002935"),
            B256::from(U256::from(21_599u64 % 8191).to_be_bytes::<32>()),
            b256!("d3c9c086c34ce7307e96051b0f2f350d01c677add81c06ac34fae98c0ac4777b"),
        );
        assert_eq!(
            b.hash(),
            b256!("2be18aa6c14f7615351f1e95e1c26c3ed5dc22defe8eecd8b53f8194dace5906")
        );
    }
}
