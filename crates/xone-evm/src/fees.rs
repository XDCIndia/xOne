//! Who is paid the transaction fee, and how much.
//!
//! geth `core/state_transition.go:881-930`, for a transaction that ran in the EVM:
//!
//! * a special transaction (to 0x89 or 0x90) credits nothing (`:881`);
//! * up to and including block `TIPTRC21Fee` (the owner rule is strictly after it, `:900`, the
//!   `EffectiveTIPTRC21FeeBlock` comparison `> 0`) the block's author receives
//!   `gasUsed * (gasPrice - baseFee)`; the base fee is subtracted only when the header has one;
//! * after it the owner of the author's masternode, read from the validator contract 0x88 in the
//!   state at the start of the block (`core/state_processor.go:96-105`; the read is
//!   `xone_state::system_contracts::validator_0x88::candidate_owner`, a crate this one does not
//!   depend on), receives the full
//!   `gasUsed * gasPrice`. With no registered owner nothing is credited: the fee does not fall back
//!   to the author (`:900-912`). The base fee is not subtracted, so none of the fee is burned.
//!
//! The author is the recovered signer of the seal for V1 (the header's coinbase is zero) and for V2
//! (geth `engine.Author`, used for `Context.Coinbase` in `core/evm.go`): [`block_author`].

use crate::{route::is_special_tx, spec::XdcRules};
use alloy_primitives::{Address, U256};
use xone_chainspec::ChainSpec;
use xone_primitives::{Header, SealError};

/// The creator of a block, which geth uses as the EVM coinbase: the seal signer of a V1 header, the
/// `validator` signature signer of a V2 header (above the V2 switch block).
pub fn block_author(spec: &ChainSpec, h: &Header) -> Result<Address, SealError> {
    let switch = spec.xdpos.v2.as_ref().map_or(u64::MAX, |v| v.switch_block);
    if h.number > switch {
        h.v2_signer()
    } else {
        h.v1_signer()
    }
}

/// The fee credit of one transaction: who receives what, or `None` for no credit.
/// `price` is the effective gas price (`msg.GasPrice`), `base_fee` the header's base fee (zero
/// when the header has none), `gas_used` the receipt's gas.
pub fn credit(
    rules: &XdcRules,
    to: Option<Address>,
    author: Address,
    owner: Option<Address>,
    price: u128,
    base_fee: u128,
    gas_used: u64,
) -> Option<(Address, U256)> {
    if is_special_tx(to) {
        return None;
    }
    let (who, per_gas) = if rules.fee_to_owner {
        (owner?, price)
    } else {
        let header_base_fee = if rules.header_has_basefee {
            base_fee
        } else {
            0
        };
        (author, price.saturating_sub(header_base_fee))
    };
    Some((who, U256::from(per_gas) * U256::from(gas_used)))
}

/// geth `params.TRC21GasPriceForBlock` (`params/xdc_trc21.go:39-50`): the gas price of a transaction
/// to a registered TRC21 token, which replaces the transaction's own. The Gas2500x tier is the base
/// fee of that tier (`XDCBaseFeeForBlock`, `params/xdc_gas.go:36-42`), from `Gas50xBlock` 12.5 gwei
/// (`common.GasPrice50x`), strictly after `TIPTRC21Fee` 0.25 gwei (`common.TRC21GasPrice`), else
/// 2500 wei (`common.TRC21GasPriceBefore`), `common/types.go:132-134`.
pub fn trc21_gas_price(spec: &ChainSpec, number: u64) -> u128 {
    use xone_chainspec::ForkBlock;
    if spec.gas2500x.is_active(number) {
        u128::from(crate::basefee::XDC_BASE_FEE * crate::basefee::GAS_2500X_MULTIPLIER)
    } else if spec.effective.gas50x.is_active(number) {
        12_500_000_000
    } else if matches!(spec.effective.tip_trc21_fee, ForkBlock::At(b) if number > b) {
        250_000_000
    } else {
        2500
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xone_chainspec::Network;

    /// The tiers of `TRC21GasPriceForBlock` on Apothem: strictly after `TIPTRC21Fee`, from
    /// `Gas50xBlock`, from `Gas2500xBlock`.
    #[test]
    fn trc21_gas_price_tiers() {
        let s = Network::Apothem.spec();
        for (n, want) in [
            (23_779_191, 2_500),
            (23_779_192, 250_000_000),
            (56_828_699, 250_000_000),
            (56_828_700, 12_500_000_000),
            (83_599_999, 12_500_000_000),
            (83_600_000, 625_000_000_000),
        ] {
            assert_eq!(trc21_gas_price(&s, n), want, "block {n}");
        }
    }
}
