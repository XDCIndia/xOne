//! The randomize contract at 0x90 (geth `common.RandomizeSMCBinary`, `common/xdc_contracts.go:28`; the V1
//! M2 order of a checkpoint reads it).
//!
//! `randomSecret` is the mapping at slot 0 from a masternode to a `bytes32[]`, `randomOpening`
//! the mapping at slot 1 to a `bytes32` (`core/state/statedb_utils.go:41-70`). Decrypting a secret
//! with its opening is `contracts/utils.go` and belongs with the V1 M2 derivation, not here.

use super::{dyn_array_slot, key, mapping_slot};
use crate::StateRead;
use alloy_primitives::{address, Address, B256, U256};

/// geth `common.RandomizeSMCBinary`, `common/xdc_contracts.go:28`.
pub const ADDRESS: Address = address!("0000000000000000000000000000000000000090");

/// `slotRandomizeMapping["randomSecret"]`, `statedb_utils.go:43`.
pub const RANDOM_SECRET: u64 = 0;
/// `slotRandomizeMapping["randomOpening"]`, `statedb_utils.go:44`.
pub const RANDOM_OPENING: u64 = 1;

fn read(s: &impl StateRead, slot: U256) -> U256 {
    s.storage(ADDRESS, B256::from(slot.to_be_bytes::<32>()))
}

/// The secrets a masternode committed, in order (geth `GetSecret`, `statedb_utils.go:48-63`).
pub fn secrets(s: &impl StateRead, masternode: Address) -> Vec<B256> {
    let slot = mapping_slot(key(masternode), RANDOM_SECRET);
    let n = read(s, slot).saturating_to::<u64>();
    (0..n)
        .map(|i| B256::from(read(s, dyn_array_slot(slot, i, 1)).to_be_bytes::<32>()))
        .collect()
}

/// The opening a masternode revealed; zero when none (geth `GetOpening`, `statedb_utils.go:65-70`).
pub fn opening(s: &impl StateRead, masternode: Address) -> B256 {
    B256::from(read(s, mapping_slot(key(masternode), RANDOM_OPENING)).to_be_bytes::<32>())
}
