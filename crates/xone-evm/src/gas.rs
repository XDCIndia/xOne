//! Gas rules that differ from stock revm: calldata price, refund cap and the intrinsic gas.

use revm::{
    context_interface::cfg::{
        gas::STANDARD_TOKEN_COST,
        gas_params::{Eip2780TxInfo, GasId, GasParams},
    },
    interpreter::InitialAndFloorGas,
    primitives::{hardfork::SpecId, U256},
};

use crate::spec::XdcRules;

/// revm gas schedule of `rules.spec_id` with the XDC prices applied.
///
/// geth `core/state_transition.go:71-80` (`XDCPreEIP2028Calldata`) and `:127-134` for the
/// calldata price; `:1343-1363` (`calcRefund`) for the refund cap.
pub fn gas_params(rules: &XdcRules) -> GasParams {
    let mut g = GasParams::new_spec(rules.spec_id);
    g.override_gas([
        (
            GasId::tx_token_non_zero_byte_multiplier(),
            rules.non_zero_calldata_gas / STANDARD_TOKEN_COST,
        ),
        (GasId::max_refund_quotient(), rules.max_refund_quotient),
    ]);
    g
}

/// The EIP-7623 floor is priced with 4 tokens per non-zero byte whatever the calldata price:
/// geth `FloorDataGas` does not read `XDCPreEIP2028Calldata`. revm shares one multiplier for the
/// intrinsic gas and the floor, so the floor comes from the stock schedule.
pub fn floor_params(rules: &XdcRules) -> GasParams {
    GasParams::new_spec(rules.spec_id)
}

/// From Amsterdam the base of the intrinsic gas and of the calldata floor is priced by what the
/// transaction does (geth `intrinsicBaseGasEIP2780`, `core/state_transition.go:200-228`): a create,
/// a self-transfer, and whether it carries value. Before it there is nothing to say.
pub fn eip2780(rules: &XdcRules, value: U256, is_self_transfer: bool) -> Option<Eip2780TxInfo> {
    (rules.spec_id >= SpecId::AMSTERDAM).then_some(Eip2780TxInfo {
        value,
        is_self_transfer,
    })
}

/// Intrinsic gas of a message: geth `IntrinsicGas` (`core/state_transition.go:84`) called with the
/// rules copy that `execute` builds (`:803-806`). Access lists and authorizations are counted by
/// number of addresses and storage keys.
pub fn intrinsic_gas(
    rules: &XdcRules,
    data: &[u8],
    create: bool,
    access_list_accounts: u64,
    access_list_storages: u64,
    eip2780: Option<Eip2780TxInfo>,
) -> InitialAndFloorGas {
    gas_params(rules).initial_tx_gas(
        data,
        create,
        access_list_accounts,
        access_list_storages,
        0,
        eip2780,
    )
}

/// The two gas tables of a block, built once: building them costs far more than pricing a message with
/// them, and the transaction pool prices every transaction it is offered (XB-16).
pub struct IntrinsicTables {
    params: GasParams,
    floor: GasParams,
}

impl IntrinsicTables {
    pub fn new(rules: &XdcRules) -> Self {
        Self {
            params: gas_params(rules),
            floor: floor_params(rules),
        }
    }

    /// `(intrinsic gas, EIP-7623 floor)` of a message, as [`intrinsic_gas`] and [`floor_params`] give them.
    pub fn of(
        &self,
        data: &[u8],
        create: bool,
        access_list_accounts: u64,
        access_list_storages: u64,
        eip2780: Option<Eip2780TxInfo>,
    ) -> (u64, u64) {
        let intrinsic = self.params.initial_tx_gas(
            data,
            create,
            access_list_accounts,
            access_list_storages,
            0,
            eip2780,
        );
        let floor = self.floor.initial_tx_gas(
            data,
            create,
            access_list_accounts,
            access_list_storages,
            0,
            eip2780,
        );
        (intrinsic.initial_total_gas(), floor.floor_gas())
    }
}
