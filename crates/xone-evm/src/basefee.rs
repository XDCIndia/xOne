//! The fixed XDC base fee.
//!
//! XDC has an EIP-1559 header field and dynamic-fee transaction types but no market: the base fee
//! is a pure function of the block number (geth `params.XDCBaseFeeForBlock`,
//! `params/xdc_gas.go:36-42`), 12.5 gwei (`params/protocol_params.go:175`), and 50 times that, 625
//! gwei, from the `Gas2500xBlock` row (`:31-32`, `params/config.go:3418`). It is absent before
//! `Eip1559Block` (`IsEIP1559`, the row alone). The tier of a block is chosen by that block's own
//! number, so a child of the last block of a tier already carries the next tier: geth `CalcBaseFee`
//! looks up `parent.Number + 1` (`consensus/misc/eip1559/eip1559.go:121-127`). Nothing is
//! derived from the parent's gas use, and the fee is credited, not burned ([`crate::fees`]).

use alloy_primitives::U256;
use xone_chainspec::ChainSpec;

/// geth `params.XDCBaseFee`, `params/protocol_params.go:175`: 12.5 gwei.
pub const XDC_BASE_FEE: u64 = 12_500_000_000;
/// geth `gas2500xMultiplier`, `params/xdc_gas.go:32`.
pub const GAS_2500X_MULTIPLIER: u64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BaseFeeError {
    /// geth `eip1559.go:66-69`: before `Eip1559Block` the header must have no base fee.
    #[error("invalid baseFee: have {have}, want <nil>")]
    Unexpected { have: U256 },
    /// geth `eip1559.go:84-86`.
    #[error("header is missing baseFee")]
    Missing,
    /// geth `eip1559.go:94-98`.
    #[error("invalid baseFee: have {have}, want {want}")]
    Wrong { have: U256, want: u64 },
}

/// geth `XDCBaseFeeForBlock` behind the `IsEIP1559` gate: what the header of block `number` must
/// carry, or `None` when it must carry none.
pub fn base_fee_for_block(spec: &ChainSpec, number: u64) -> Option<u64> {
    spec.is_eip1559(number)
        .then(|| xdc_base_fee_for_block(spec, number))
}

/// geth `params.XDCBaseFeeForBlock` itself (`params/xdc_gas.go:36-42`): the tier of block
/// `number`, without the `Eip1559Block` gate (its callers check what they need, e.g. the London
/// gate of `eth_feeHistory`'s next fee, `eth/gasprice/feehistory.go:93-97`).
pub fn xdc_base_fee_for_block(spec: &ChainSpec, number: u64) -> u64 {
    if spec.gas2500x.is_active(number) {
        XDC_BASE_FEE * GAS_2500X_MULTIPLIER
    } else {
        XDC_BASE_FEE
    }
}

/// geth `CalcBaseFee` for the child of block `parent_number` (`eip1559.go:121-127`).
pub fn child_base_fee(spec: &ChainSpec, parent_number: u64) -> Option<u64> {
    base_fee_for_block(spec, parent_number + 1)
}

/// geth `VerifyEIP1559Header` for an XDC chain (`eip1559.go:63-99`), the base-fee part: an exact
/// comparison with the block's tier, without the parent. The gas-limit check is the header verifier's.
pub fn verify(spec: &ChainSpec, number: u64, base_fee: Option<U256>) -> Result<(), BaseFeeError> {
    match (base_fee_for_block(spec, number), base_fee) {
        (None, None) => Ok(()),
        (None, Some(have)) => Err(BaseFeeError::Unexpected { have }),
        (Some(_), None) => Err(BaseFeeError::Missing),
        (Some(want), Some(have)) if have == U256::from(want) => Ok(()),
        (Some(want), Some(have)) => Err(BaseFeeError::Wrong { have, want }),
    }
}
