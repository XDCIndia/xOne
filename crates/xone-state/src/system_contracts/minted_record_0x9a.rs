//! The MintedRecord contract at 0x9a (geth `common.MintedRecordAddressBinary`,
//! `common/xdc_contracts.go:58`), written by the tiered reward path of every epoch switch from
//! `TIPFixedReward`. Slot layout: `core/state/statedb_utils.go:160-231`. These slots are consensus
//! state: they are part of the state root.

use alloy_primitives::{address, Address, B256, U256};

pub const ADDRESS: Address = address!("000000000000000000000000000000000000009a");

/// `slotMintedRecordOnsetEpoch` and `slotMintedRecordOnsetBlock`, `statedb_utils.go:172-173`.
pub fn onset_epoch() -> B256 {
    B256::with_last_byte(1)
}

pub fn onset_block() -> B256 {
    B256::with_last_byte(2)
}

/// `base + epoch` with the bases of `statedb_utils.go:174-176` (`0x01`, `0x02`, `0x03` in the top byte).
fn per_epoch(top: u8, epoch: u64) -> B256 {
    B256::from(((U256::from(top) << 248usize) + U256::from(epoch)).to_be_bytes::<32>())
}

pub fn post_minted(epoch: u64) -> B256 {
    per_epoch(1, epoch)
}

pub fn post_burned(epoch: u64) -> B256 {
    per_epoch(2, epoch)
}

pub fn post_reward_block(epoch: u64) -> B256 {
    per_epoch(3, epoch)
}
