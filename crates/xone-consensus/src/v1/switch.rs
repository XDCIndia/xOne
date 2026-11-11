//! The hand-off from V1 to V2. The switch block is epoch-aligned, is the last V1 block, and is a
//! V1 checkpoint: its `extraData` lists the masternodes of the first V2 epoch.

use alloy_primitives::Address;
use xone_chainspec::XdposConfig;
use xone_primitives::Header;

use super::checkpoint::masternodes_of;
use crate::verify::HeaderError;

/// geth `New` panics unless `SwitchBlock % Epoch == 0` (`consensus/XDPoS/xdpos.go:502-505`); here
/// it is a typed error for the caller to raise at start-up.
pub fn check_switch_block(x: &XdposConfig) -> Result<(), HeaderError> {
    let Some(v2) = &x.v2 else { return Ok(()) };
    match v2.switch_block.checked_rem(x.epoch) {
        None => Err(HeaderError::ZeroEpoch),
        Some(0) => Ok(()),
        Some(_) => Err(HeaderError::SwitchBlockNotEpochAligned {
            switch_block: v2.switch_block,
            epoch: x.epoch,
        }),
    }
}

/// geth `IsV2Block` (`consensus/XDPoS/xdpos.go:530-533`): strictly above the switch block. The
/// switch block itself is a V1 block.
pub fn is_v2_block(number: u64, switch_block: Option<u64>) -> bool {
    switch_block.is_some_and(|s| number > s)
}

/// The masternodes of the first V2 epoch: read verbatim from the V1 `extraData` of the switch
/// block (`GetMasternodesFromCheckpointHeader`, `consensus/XDPoS/xdpos.go:2122-2127`).
pub fn switch_masternodes(switch_header: &Header) -> Result<Vec<Address>, HeaderError> {
    let masternodes = masternodes_of(&switch_header.extra_data)?;
    if masternodes.is_empty() {
        return Err(HeaderError::MissingMasternodes);
    }
    Ok(masternodes)
}
