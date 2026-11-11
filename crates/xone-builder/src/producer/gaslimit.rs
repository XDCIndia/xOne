//! The gas limit and the base fee of a block this node builds (`spec/producer/gas-limit.md`).
//!
//! geth `xdcGasLimit` (`miner/xdpos_worker.go:320-331`) and `core.CalcGasLimit`
//! (`core/block_validator.go:266-287`). Three rows of one chain:
//!
//! * no `DynamicGasLimitBlock` row (geth `nil`, mainnet): `CalcGasLimit(parent, max(420M, parent))`, the
//!   plateau rule (`xdcGasCeil`, `:333-338`);
//! * the row set, the child below it (`Unscheduled` included, which geth holds as a far block): the
//!   static ceiling of the miner, whatever the parent;
//! * the row set, the child at or above it: `CalcGasLimit(parent, ceiling)`.
//!
//! The base fee in the header is [`xone_evm::basefee`]: the 2500x tier from its own row.

use xone_chainspec::{ChainSpec, ForkBlock};

/// geth `params.XDCBlockGasLimit`, `params/protocol_params.go:37`.
pub const XDC_BLOCK_GAS_LIMIT: u64 = 420_000_000;
/// geth `params.GasLimitBoundDivisor`, `params/protocol_params.go:26`.
const BOUND_DIVISOR: u64 = 1024;
/// geth `params.MinGasLimit`, `params/protocol_params.go:27`.
const MIN_GAS_LIMIT: u64 = 5000;

/// geth `core.CalcGasLimit`: one step of at most `parent / 1024 - 1` toward `desired`. Go's `uint64`
/// arithmetic wraps, and so does this.
pub fn calc_gas_limit(parent: u64, desired: u64) -> u64 {
    let delta = (parent / BOUND_DIVISOR).wrapping_sub(1);
    let desired = desired.max(MIN_GAS_LIMIT);
    if parent < desired {
        return parent.wrapping_add(delta).min(desired);
    }
    if parent > desired {
        return parent.wrapping_sub(delta).max(desired);
    }
    parent
}

/// geth `xdcGasCeil` (`miner/xdpos_worker.go:333-338`): never aim below the parent's plateau.
pub fn plateau_ceil(parent: u64) -> u64 {
    parent.max(XDC_BLOCK_GAS_LIMIT)
}

/// The gas limit of block `number`, the child of a block with gas limit `parent`. `ceil` is the miner's
/// static ceiling (`--miner.gaslimit`, geth `Config.GasCeil`); it is read only on a chain with the row.
pub fn gas_limit(spec: &ChainSpec, parent: u64, number: u64, ceil: u64) -> u64 {
    match spec.dynamic_gas_limit {
        ForkBlock::Never => calc_gas_limit(parent, plateau_ceil(parent)),
        row if row.is_active(number) => calc_gas_limit(parent, ceil),
        _ => ceil,
    }
}

/// The header base fee of block `number`: the fixed tier of that number, none before `Eip1559Block`.
pub fn base_fee(spec: &ChainSpec, number: u64) -> Option<u64> {
    xone_evm::basefee::base_fee_for_block(spec, number)
}
