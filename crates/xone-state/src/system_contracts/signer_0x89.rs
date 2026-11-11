//! The block-signer contract at 0x89 (geth `common.BlockSignersBinary`, `common/xdc_contracts.go:19`).
//!
//! `blockSigners` is the mapping at slot 0 from a block hash to an address array
//! (`core/state/statedb_utils.go:16-19`). The contract is wiped at `TIPSigning`
//! (`core/state_processor.go:321-327`), so after that block every read is empty.

use super::{address_of, dyn_array_slot, mapping_slot};
use crate::StateRead;
use alloy_primitives::{address, Address, B256, U256};

/// geth `common.BlockSignersBinary`, `common/xdc_contracts.go:19`.
pub const ADDRESS: Address = address!("0000000000000000000000000000000000000089");

/// `slotBlockSignerMapping["blockSigners"]`, `statedb_utils.go:17`.
pub const BLOCK_SIGNERS: u64 = 0;

fn read(s: &impl StateRead, slot: U256) -> U256 {
    s.storage(ADDRESS, B256::from(slot.to_be_bytes::<32>()))
}

/// The masternodes that signed the block with this hash, in contract order (geth `GetSigners`,
/// `statedb_utils.go:22-40`).
pub fn signers(s: &impl StateRead, block_hash: B256) -> Vec<Address> {
    let slot = mapping_slot(block_hash, BLOCK_SIGNERS);
    let n = read(s, slot).saturating_to::<u64>();
    (0..n)
        .map(|i| address_of(read(s, dyn_array_slot(slot, i, 1))))
        .collect()
}
