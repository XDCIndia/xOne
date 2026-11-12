//! Which revm `SpecId` and which XDC couplings apply at a block.
//!
//! geth-XDC does not follow Ethereum's fork order: Istanbul rules start at
//! `TIPXDCXCancellationFee`, Berlin, London, Merge and Shanghai are marker forks that only add
//! opcodes, and the EIP-2929/3529/2565/3860 bundle starts at `Eip1559Block`. revm has one
//! `SpecId` per Ethereum fork, so the XDC rules are a `SpecId` plus the [`XdcRules`] below.

use alloy_primitives::{keccak256, B256};
use revm::{
    context::{BlockEnv, CfgEnv},
    primitives::{eip4844, hardfork::SpecId},
};
use xone_chainspec::{ChainSpec, ForkBlock};

use crate::gas;

/// What the BASEFEE opcode returns while the header carries no base fee (London to `Eip1559Block`).
/// geth-XDC `params/xdc_gas.go:48` (`XDCBaseFeeForOpcode`), value `params/protocol_params.go:175`.
pub const BASEFEE_OPCODE_WITHOUT_HEADER_FEE: u64 = 12_500_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpecError {
    #[error("fork `{0}` is active at this block and xone-evm has no rules for it")]
    UnsupportedFork(&'static str),
}

/// The XDC couplings at one block. Every field is derived from the chain spec by [`rules`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdcRules {
    pub chain_id: u64,
    /// The revm fork whose instruction table and gas schedule apply.
    pub spec_id: SpecId,
    /// EIP-2929, 3529, 2565, 3860 and 3541 together, from `Eip1559Block` (geth `Rules.IsEIP1559`).
    pub eip1559_bundle: bool,
    /// BASEFEE, PREVRANDAO (0x44) and PUSH0 are live although `spec_id` is below London, because
    /// geth-XDC drives the London, Merge and Shanghai markers from `londonBlock`.
    pub marker_opcodes: bool,
    /// Gas refund cap is `gas_used / max_refund_quotient`: 2 until `Eip1559Block`, then 5.
    pub max_refund_quotient: u64,
    /// Gas per non-zero calldata byte: 68, or 16 from Prague (the devnet that keeps 68 stays at 68).
    pub non_zero_calldata_gas: u64,
    /// STATICCALL touches the callee (zero-value transfer) only from the Istanbul rules.
    pub static_call_touch: bool,
    /// MODEXP is priced by EIP-2565.
    pub modexp_eip2565: bool,
    /// The XDCx precompiles 0x1e, 0x28, 0x29 and 0x2a exist.
    pub xdcx_precompiles: bool,
    /// Opcode 0x44 returns `keccak256(block number)` (PREVRANDAO); before, the header difficulty.
    pub prevrandao_is_number_hash: bool,
    /// The header carries a base fee (from `Eip1559Block`).
    pub header_has_basefee: bool,
    /// The transaction fee goes to the coinbase owner (TRC21 routing), not to the coinbase.
    pub fee_to_owner: bool,
    /// A transaction to 0x89 is a sign transaction: no EVM (geth `IsTIPSigning`, `>=`).
    pub sign_tx_route: bool,
    /// A transaction to 0x91 to 0x94 is an empty transaction (geth `IsTIPXDCXReceiver`).
    pub xdcx_empty_route: bool,
}

fn active_after(f: ForkBlock, n: u64) -> bool {
    matches!(f, ForkBlock::At(at) if n > at)
}

/// geth `Rules` plus `core/vm/evm.go:182-226` (instruction set choice), per block number.
pub fn rules(spec: &ChainSpec, n: u64) -> Result<XdcRules, SpecError> {
    let bundle = spec.is_eip1559(n);
    let marker_era = spec.berlin.is_active(n) || spec.london.is_active(n);
    // geth `core/vm/evm.go:172-180`: Amsterdam, Osaka and Prague each have their own instruction set.
    let spec_id = if spec.amsterdam.is_active(n) {
        SpecId::AMSTERDAM
    } else if spec.osaka.is_active(n) {
        SpecId::OSAKA
    } else if spec.prague.is_active(n) {
        SpecId::PRAGUE
    } else if spec.cancun.is_active(n) {
        SpecId::CANCUN
    } else if bundle {
        SpecId::SHANGHAI
    } else if spec.is_istanbul(n) || marker_era {
        SpecId::ISTANBUL
    } else if spec.is_petersburg(n) || spec.constantinople.is_active(n) {
        SpecId::PETERSBURG
    } else if spec.byzantium.is_active(n) {
        SpecId::BYZANTIUM
    } else if spec.eip158.is_active(n) {
        SpecId::SPURIOUS_DRAGON
    } else if spec.eip150.is_active(n) {
        SpecId::TANGERINE
    } else if spec.homestead.is_active(n) {
        SpecId::HOMESTEAD
    } else {
        SpecId::FRONTIER
    };
    let london = spec.london.is_active(n);
    // geth `activePrecompiledContracts`: Byzantium to `TIPXDCXMinerDisable`, never with the
    // EIP-1559 Berlin set, Cancun or Prague.
    let xdcx_ended = spec.effective.tip_xdcx_miner_disable.is_active(n)
        || (spec.berlin.is_active(n) && bundle)
        || spec.cancun.is_active(n);
    let xdcx = spec.byzantium.is_active(n) && !xdcx_ended;
    Ok(XdcRules {
        chain_id: spec.chain_id,
        spec_id,
        eip1559_bundle: bundle,
        marker_opcodes: london && spec_id < SpecId::LONDON,
        max_refund_quotient: if bundle { 5 } else { 2 },
        non_zero_calldata_gas: if spec.is_pre_eip2028_calldata(n) {
            68
        } else {
            16
        },
        static_call_touch: spec.is_istanbul(n),
        modexp_eip2565: (bundle && spec.berlin.is_active(n)) || spec.cancun.is_active(n),
        xdcx_precompiles: xdcx,
        prevrandao_is_number_hash: london,
        header_has_basefee: bundle,
        fee_to_owner: active_after(spec.effective.tip_trc21_fee, n),
        sign_tx_route: spec.effective.tip_signing.is_active(n),
        xdcx_empty_route: spec.is_tip_xdcx_receiver(n),
    })
}

/// `keccak256` of the block number as a minimal big-endian integer. geth `core/evm.go:68-73`.
pub fn prevrandao(number: u64) -> B256 {
    let b = number.to_be_bytes();
    let skip = b.iter().take_while(|x| **x == 0).count();
    keccak256(&b[skip..])
}

/// The block fields geth feeds to the EVM, before the XDC couplings are applied.
#[derive(Debug, Clone, Default)]
pub struct XdcBlock {
    pub number: u64,
    pub timestamp: u64,
    pub beneficiary: alloy_primitives::Address,
    pub gas_limit: u64,
    pub difficulty: alloy_primitives::U256,
    /// The header's `baseFeePerGas`, if it has one.
    pub base_fee: Option<u64>,
}

impl XdcRules {
    /// revm `BlockEnv`: PREVRANDAO and BASEFEE as geth-XDC reports them.
    pub fn block_env(&self, b: &XdcBlock) -> BlockEnv {
        let basefee = match b.base_fee {
            Some(f) if self.header_has_basefee => f,
            _ if self.marker_opcodes => BASEFEE_OPCODE_WITHOUT_HEADER_FEE,
            _ => 0,
        };
        let mut env = BlockEnv {
            number: alloy_primitives::U256::from(b.number),
            beneficiary: b.beneficiary,
            timestamp: alloy_primitives::U256::from(b.timestamp),
            gas_limit: b.gas_limit,
            basefee,
            difficulty: b.difficulty,
            prevrandao: self.prevrandao_is_number_hash.then(|| prevrandao(b.number)),
            blob_excess_gas_and_price: None,
            slot_num: 0,
        };
        if self.spec_id >= SpecId::CANCUN {
            // XDC has no blobs and geth never sets a blob base fee; revm wants one from Cancun on.
            env.set_blob_excess_gas_and_price(0, eip4844::BLOB_BASE_FEE_UPDATE_FRACTION_PRAGUE);
        }
        env
    }

    /// revm `CfgEnv` for this block.
    pub fn cfg(&self) -> CfgEnv {
        let mut cfg = CfgEnv::new_with_spec(self.spec_id);
        cfg.chain_id = self.chain_id;
        cfg.set_gas_params(gas::gas_params(self));
        cfg
    }
}
