//! Typed readers of the XDC system contracts at 0x88 (validator), 0x89 (block signers) and
//! 0x90 (randomize), over [`crate::StateRead`].
//!
//! The slot arithmetic is geth `core/state/state_reader.go:13-41` (`GetLocMappingAtKey`,
//! `GetLocDynamicArrAtElement`), which is the Solidity storage layout of the three contracts.
//! The slot numbers are geth `core/state/statedb_utils.go`, taken from the compiled contract
//! ("KYC Layer added", `statedb_utils.go:72-73`). Each reader only reads; a word read as an address
//! is the low 20 bytes of the 32-byte word (geth `common.HexToAddress(word.Hex())`).

use alloy_primitives::{keccak256, Address, B256, U256};

pub mod randomize_0x90;
pub mod signer_0x89;
pub mod validator_0x88;

/// Storage key of `mapping[key]` declared at `slot`: `keccak256(pad32(key) ++ pad32(slot))`
/// (geth `GetLocMappingAtKey`, `state_reader.go:18-24`).
pub fn mapping_slot(key: B256, slot: u64) -> U256 {
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(key.as_slice());
    b[32..].copy_from_slice(&U256::from(slot).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(b).0)
}

/// Storage key of element `i` of a dynamic array whose length is at `slot`:
/// `keccak256(pad32(slot)) + i * size` (geth `GetLocDynamicArrAtElement`, `state_reader.go:26-31`).
/// geth adds as a big integer and keeps the low 32 bytes, which is a wrapping add.
pub fn dyn_array_slot(slot: U256, i: u64, size: u64) -> U256 {
    U256::from_be_bytes(keccak256(slot.to_be_bytes::<32>()).0)
        .wrapping_add(U256::from(i).wrapping_mul(U256::from(size)))
}

pub(crate) fn key(a: Address) -> B256 {
    a.into_word()
}

pub(crate) fn word(v: U256) -> B256 {
    B256::from(v.to_be_bytes::<32>())
}

/// The address in the low 20 bytes of a word.
pub(crate) fn address_of(v: U256) -> Address {
    Address::from_word(word(v))
}
