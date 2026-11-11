//! Which handler a transaction goes through: geth `RouteTransaction`
//! (`core/state_processor.go:856-872`), which block processing and block building share.
//!
//! Only the target and the block number decide. There is no check of the sender, the gas price or
//! the calldata here: that a sign transaction comes from a masternode, costs nothing and carries
//! 68 bytes of `sign(uint256,bytes32)` is txpool admission (`core/txpool/legacypool/legacypool.go:
//! hasKnownSpecialCalldata`, XB-16) and the reward count (`consensus/XDPoS/reward.go:56`, XB-10).
//! A sign transaction in a block is applied whatever its sender and calldata.

use crate::spec::XdcRules;
use alloy_primitives::{address, Address};

/// geth `common.BlockSignersBinary`, `common/xdc_contracts.go:19`.
pub const BLOCK_SIGNERS: Address = address!("0000000000000000000000000000000000000089");
/// geth `common.RandomizeSMCBinary`, `common/xdc_contracts.go:28`.
pub const RANDOMIZE: Address = address!("0000000000000000000000000000000000000090");
/// geth `common.XDCXAddrBinary`, `common/xdc_contracts.go:38`.
pub const XDCX: Address = address!("0000000000000000000000000000000000000091");
/// geth `common.TradingStateAddrBinary`, `common/xdc_contracts.go:41`.
pub const TRADING_STATE: Address = address!("0000000000000000000000000000000000000092");
/// geth `common.XDCXLendingAddressBinary`, `common/xdc_contracts.go:44`.
pub const XDCX_LENDING: Address = address!("0000000000000000000000000000000000000093");
/// geth `common.XDCXLendingFinalizedTradeAddressBinary`, `common/xdc_contracts.go:48`.
pub const XDCX_LENDING_FINALIZED_TRADE: Address =
    address!("0000000000000000000000000000000000000094");

/// What block processing does with a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// To 0x89 from `TIPSigning` on: no EVM, no gas, nonce bump and a minimal log
    /// ([`crate::special::apply_sign_tx`]).
    SignTx,
    /// To 0x90: runs in the EVM like any call, but is a special transaction (no fee floor, no fee
    /// credit, see [`is_special_tx`]).
    RandomizeTx,
    /// To 0x91 to 0x94 inside the `TIPXDCX` receiver window (ADR-009, issue 23: xOne routes them as
    /// geth-XDC does and runs no XDCx engine): no EVM, no gas, no nonce bump
    /// ([`crate::special::apply_empty_tx`]).
    XdcxEmpty,
    /// The EVM. Includes a contract creation and, before `TIPSigning`, a call to 0x89.
    Evm,
}

impl Route {
    pub fn runs_evm(self) -> bool {
        matches!(self, Self::RandomizeTx | Self::Evm)
    }
}

/// geth `types.IsSpecialTx`, `core/types/transaction.go:740-742`: a target of 0x89 or 0x90, at any
/// block. Such a transaction skips the fee-cap floor (`core/state_transition.go:684`) and credits no
/// fee (`:881`), including a 0x89 call that still runs in the EVM before `TIPSigning`.
pub fn is_special_tx(to: Option<Address>) -> bool {
    to.is_some_and(|t| t == BLOCK_SIGNERS || t == RANDOMIZE)
}

/// geth `RouteTransaction`, `core/state_processor.go:856-872`, in the same order of checks: a
/// creation is EVM; 0x89 from `TIPSigning` (`IsTIPSigning`, `params/config.go:3299`, `>=`) is a sign
/// transaction; 0x91 to 0x94 while `IsTIPXDCXReceiver` is an empty transaction. Both switches are
/// fields of [`XdcRules`], which [`crate::spec::rules`] reads from the chain spec.
pub fn route(rules: &XdcRules, to: Option<Address>) -> Route {
    let Some(to) = to else { return Route::Evm };
    if to == BLOCK_SIGNERS && rules.sign_tx_route {
        return Route::SignTx;
    }
    if [
        XDCX,
        TRADING_STATE,
        XDCX_LENDING,
        XDCX_LENDING_FINALIZED_TRADE,
    ]
    .contains(&to)
        && rules.xdcx_empty_route
    {
        return Route::XdcxEmpty;
    }
    if to == RANDOMIZE {
        return Route::RandomizeTx;
    }
    Route::Evm
}
