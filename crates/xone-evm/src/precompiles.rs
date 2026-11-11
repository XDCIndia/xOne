//! The precompile set per fork row.
//!
//! geth `core/vm/contracts.go:110-120` (`PrecompiledContractsXDCBerlin`, MODEXP without EIP-2565),
//! `:240-286` (selection), `core/vm/contracts_xdc.go:92-125` (Cancun, Prague) and `:144-206` (the
//! XDCx precompiles, present until `TIPXDCXMinerDisable`). The set is a pure function of the chain
//! spec and the block number; it does not read the revm `SpecId`.

use alloy_primitives::{Address, Bytes};
use revm::{
    context_interface::{Cfg, ContextTr},
    handler::PrecompileProvider,
    interpreter::{CallInputs, Gas, InstructionResult, InterpreterResult},
    precompile::{
        blake2, bls12_381, bn254, modexp, u64_to_address, Precompile, PrecompileError,
        PrecompileHalt, PrecompileId, PrecompileOutput, PrecompileResult, PrecompileStatus,
        Precompiles,
    },
    primitives::AddressSet,
};
use std::collections::BTreeSet;
use xone_chainspec::ChainSpec;

use crate::spec::XdcRules;

const XDCX_RING_VERIFIER: Address = u64_to_address(0x1e);
const XDCX_BULLETPROOF: Address = u64_to_address(0x28);
const XDCX_LAST_PRICE: Address = u64_to_address(0x29);
const XDCX_EPOCH_PRICE: Address = u64_to_address(0x2a);
const KZG_POINT_EVALUATION: Address = u64_to_address(0x0a);

/// geth `core/vm/contracts_xdc.go:144-158`: 1 gas, 32 zero bytes.
fn xdcx_price(_: &[u8], gas_limit: u64, reservoir: u64) -> PrecompileResult {
    if gas_limit < 1 {
        return Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir));
    }
    Ok(PrecompileOutput::new(
        1,
        Bytes::from(vec![0u8; 32]),
        reservoir,
    ))
}

/// 0x1e and 0x28 verify ring signatures and bulletproofs (geth `core/vm/privacy`). The proof
/// systems are not ported: the call is a fatal error, never a silent success or failure.
fn xdcx_privacy(_: &[u8], _: u64, _: u64) -> PrecompileResult {
    Err(PrecompileError::Fatal(
        "XDCx privacy precompile (0x1e, 0x28) is not implemented".into(),
    ))
}

/// geth `core/vm/contracts_xdc.go:53-58`: costs the point evaluation gas and always fails.
fn kzg_unsupported(_: &[u8], _: u64, reservoir: u64) -> PrecompileResult {
    Ok(PrecompileOutput::halt(
        PrecompileHalt::Other("kzg point evaluation is not supported on XDC".into()),
        reservoir,
    ))
}

fn custom(name: &str, a: Address, f: revm::precompile::PrecompileFn) -> Precompile {
    Precompile::new(PrecompileId::Custom(name.to_owned().into()), a, f)
}

/// Which precompiles exist at a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrecompileRow {
    pub byzantium: bool,
    pub istanbul: bool,
    pub prague: bool,
    pub modexp_eip2565: bool,
    pub xdcx: bool,
}

/// geth `activePrecompiledContracts`, per rows of the chain spec.
pub fn row(spec: &ChainSpec, rules: &XdcRules, n: u64) -> PrecompileRow {
    PrecompileRow {
        byzantium: spec.byzantium.is_active(n),
        istanbul: spec.is_istanbul(n) || spec.berlin.is_active(n) || spec.cancun.is_active(n),
        prague: spec.prague.is_active(n),
        modexp_eip2565: rules.modexp_eip2565,
        xdcx: rules.xdcx_precompiles,
    }
}

impl PrecompileRow {
    fn build(&self) -> Precompiles {
        let mut p = Precompiles::homestead().clone();
        if self.byzantium {
            p.extend([
                if self.modexp_eip2565 {
                    modexp::BERLIN
                } else {
                    modexp::BYZANTIUM
                },
                bn254::add::BYZANTIUM,
                bn254::mul::BYZANTIUM,
                bn254::pair::BYZANTIUM,
            ]);
        }
        if self.istanbul {
            p.extend([
                bn254::add::ISTANBUL,
                bn254::mul::ISTANBUL,
                bn254::pair::ISTANBUL,
                blake2::FUN,
            ]);
        }
        if self.prague {
            p.extend(bls12_381::precompiles());
            p.extend([custom(
                "KZG_POINT_EVALUATION_UNSUPPORTED",
                KZG_POINT_EVALUATION,
                kzg_unsupported,
            )]);
        }
        if self.xdcx {
            p.extend([
                custom("RING_SIG_VERIFY", XDCX_RING_VERIFIER, xdcx_privacy),
                custom("BULLET_PROOF_VERIFY", XDCX_BULLETPROOF, xdcx_privacy),
                custom("XDCX_LAST_PRICE", XDCX_LAST_PRICE, xdcx_price),
                custom("XDCX_EPOCH_PRICE", XDCX_EPOCH_PRICE, xdcx_price),
            ]);
        }
        p
    }

    /// Addresses a call runs as precompiles: the key set of geth `ActivePrecompiledContracts`.
    pub fn executing(&self) -> BTreeSet<Address> {
        self.build().addresses().copied().collect()
    }

    /// Addresses geth warms in the access list: `ActivePrecompiles`. It does not list the XDCx
    /// precompiles (geth `contracts_xdc.go:109-125`).
    pub fn warm(&self) -> BTreeSet<Address> {
        let xdcx = [
            XDCX_RING_VERIFIER,
            XDCX_BULLETPROOF,
            XDCX_LAST_PRICE,
            XDCX_EPOCH_PRICE,
        ];
        let mut s = self.executing();
        s.retain(|a| !xdcx.contains(a));
        s
    }

    /// Run one precompile with unlimited gas; `None` if the address is not a precompile here.
    pub fn run(&self, address: &Address, input: &[u8]) -> Option<PrecompileResult> {
        self.build()
            .get(address)
            .map(|p| p.execute(input, u64::MAX, 0))
    }
}

/// What stock revm does with a precompile output (`revm-handler` `precompile_provider.rs`, private):
/// success and revert keep their output and unspent gas, any halt burns all gas.
fn to_interpreter_result(out: PrecompileOutput, gas_limit: u64) -> InterpreterResult {
    let result = if out.gas_used > gas_limit {
        InstructionResult::PrecompileOOG
    } else {
        match &out.status {
            PrecompileStatus::Success => InstructionResult::Return,
            PrecompileStatus::Revert => InstructionResult::Revert,
            PrecompileStatus::Halt(h) if h.is_oog() => InstructionResult::PrecompileOOG,
            PrecompileStatus::Halt(_) => InstructionResult::PrecompileError,
        }
    };
    let mut gas = Gas::new(gas_limit);
    *gas.tracker_mut() = out.to_gas_tracker(gas_limit);
    if result.is_halt() {
        gas.spend_all();
        return InterpreterResult::new(result, Default::default(), gas);
    }
    InterpreterResult::new(result, out.bytes, gas)
}

/// revm `PrecompileProvider` over a [`PrecompileRow`]. `set_spec` is a no-op: the set does not
/// depend on the revm `SpecId`.
#[derive(Debug)]
pub struct XdcPrecompiles {
    set: Precompiles,
    warm: AddressSet,
}

impl XdcPrecompiles {
    pub fn new(row: &PrecompileRow) -> Self {
        let set = row.build();
        let mut warm = AddressSet::default();
        for a in row.warm() {
            warm.insert(a);
        }
        Self { set, warm }
    }
}

impl<CTX: ContextTr> PrecompileProvider<CTX> for XdcPrecompiles {
    type Output = InterpreterResult;

    fn set_spec(&mut self, _: <CTX::Cfg as Cfg>::Spec) -> bool {
        false
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<InterpreterResult>, String> {
        let Some(p) = self.set.get(&inputs.bytecode_address) else {
            return Ok(None);
        };
        let out = p
            .execute(
                &inputs.input.as_bytes(context),
                inputs.gas_limit,
                inputs.reservoir,
            )
            .map_err(|e| e.to_string())?;
        Ok(Some(to_interpreter_result(out, inputs.gas_limit)))
    }

    fn warm_addresses(&self) -> &AddressSet {
        &self.warm
    }

    /// A call to an XDCx address must run it even though it is not warmed.
    fn contains(&self, address: &Address) -> bool {
        self.set.contains(address)
    }
}
