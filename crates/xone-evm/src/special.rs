//! Transactions that do not run in the EVM, and the `TIPSigning` block rule.
//!
//! * [`apply_sign_tx`]: geth `ApplySigningTransaction`, `core/state_processor.go:883-933`;
//! * [`apply_empty_tx`]: geth `ApplyEmptyTransaction`, `core/state_processor.go:943-975`;
//! * [`tip_signing_diff`]: geth `ApplyTIPSigningHardFork`, `core/state_processor.go:321-327`.
//!
//! Both transaction paths use no gas, so `usedGas` of the block does not change and the receipt
//! carries the running `cumulativeGasUsed` unchanged. The receipt is a legacy receipt whatever the
//! transaction type (`state_processor.go:917-927`): the type decides the receipt-trie encoding.

use crate::route::BLOCK_SIGNERS;
use alloy_primitives::{logs_bloom, Address, Bloom, Log, LogData};
use alloy_rlp::{Encodable, Header};
use xone_chainspec::{ChainSpec, ForkBlock};

/// The consensus fields of a receipt (geth `types.Receipt`, `core/types/receipt.go`). A log keeps
/// only address, topics and data: block number, hashes and indices are derived, not encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// Transaction type, which is the receipt-trie prefix byte when not zero.
    pub tx_type: u8,
    pub success: bool,
    pub cumulative_gas_used: u64,
    pub gas_used: u64,
    pub logs: Vec<Log>,
    pub bloom: Bloom,
}

impl Receipt {
    pub fn new(
        tx_type: u8,
        success: bool,
        cumulative_gas_used: u64,
        gas_used: u64,
        logs: Vec<Log>,
    ) -> Self {
        let bloom = logs_bloom(logs.iter());
        Self {
            tx_type,
            success,
            cumulative_gas_used,
            gas_used,
            logs,
            bloom,
        }
    }

    /// The bytes a receipt trie holds (geth `Receipt.MarshalBinary`): `rlp([status,
    /// cumulativeGasUsed, bloom, logs])`, behind the type byte for a typed receipt. A post-state
    /// root instead of a status exists only before Byzantium, which no route here can reach.
    pub fn encode(&self) -> Vec<u8> {
        let status = u64::from(self.success);
        let payload_length = status.length()
            + self.cumulative_gas_used.length()
            + self.bloom.length()
            + self.logs.length();
        let mut out = Vec::new();
        if self.tx_type != 0 {
            out.push(self.tx_type);
        }
        Header {
            list: true,
            payload_length,
        }
        .encode(&mut out);
        status.encode(&mut out);
        self.cumulative_gas_used.encode(&mut out);
        self.bloom.encode(&mut out);
        self.logs.encode(&mut out);
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecialError {
    /// geth `state_processor.go:894-897`.
    #[error("signing tx nonce mismatch: state={state} tx={tx}")]
    NonceMismatch { state: u64, tx: u64 },
    /// Before Byzantium geth writes an intermediate state root into the receipt. `TIPSigning` and
    /// `TIPXDCX` come after Byzantium on every network, so xone does not build that receipt.
    #[error("receipt of a special transaction before Byzantium needs an intermediate root")]
    PreByzantium,
}

/// A log with only an address: no topics, no data (geth builds `types.Log{Address, BlockNumber}`).
fn minimal_log(address: Address) -> Log {
    Log {
        address,
        data: LogData::default(),
    }
}

/// Apply a sign transaction (a [`crate::route::Route::SignTx`]): no EVM, no gas, the sender's nonce
/// goes up by one, and one log `{Address: 0x89}` is emitted. `state_nonce` is the sender's nonce
/// before the transaction (0 for an account that does not exist) and must equal the transaction's.
/// The log has no topics and no data: the `Sign` event of the contract never fires, as the geth
/// comment at `state_processor.go:899-905` records. `cumulative_gas_used` is the block's running
/// total, passed through (`gp.Used()`, `state_processor.go:236`). Returns the sender's new nonce,
/// the only state change, and the receipt.
pub fn apply_sign_tx(
    spec: &ChainSpec,
    number: u64,
    state_nonce: u64,
    tx_nonce: u64,
    cumulative_gas_used: u64,
) -> Result<(u64, Receipt), SpecialError> {
    if !spec.byzantium.is_active(number) {
        return Err(SpecialError::PreByzantium);
    }
    if state_nonce != tx_nonce {
        return Err(SpecialError::NonceMismatch {
            state: state_nonce,
            tx: tx_nonce,
        });
    }
    let receipt = Receipt::new(
        0,
        true,
        cumulative_gas_used,
        0,
        vec![minimal_log(BLOCK_SIGNERS)],
    );
    Ok((state_nonce + 1, receipt))
}

/// Apply an XDCx empty transaction (a [`crate::route::Route::XdcxEmpty`]): no state change at all,
/// not even the nonce, and one log `{Address: to}` (geth `ApplyEmptyTransaction`, which the
/// ADR-009 working assumption adopts: xOne runs no XDCx engine).
pub fn apply_empty_tx(
    spec: &ChainSpec,
    number: u64,
    to: Address,
    cumulative_gas_used: u64,
) -> Result<Receipt, SpecialError> {
    if !spec.byzantium.is_active(number) {
        return Err(SpecialError::PreByzantium);
    }
    Ok(Receipt::new(
        0,
        true,
        cumulative_gas_used,
        0,
        vec![minimal_log(to)],
    ))
}

/// The `TIPSigning` block rule: at exactly the block where `IsTIPSigning` first holds
/// (`EffectiveTIPSigningBlock().Cmp(number) == 0`, `params/config.go:3307`), before the first
/// transaction of the block, contract 0x89 is self-destructed and removed. The sequence in geth is
/// `IntermediateRoot(false)`, `SelfDestruct(0x89)`, `Finalise(true)`, `IntermediateRoot(true)`
/// (`state_processor.go:321-327`). The first root is discarded: it only opens the trie for the
/// path database, so the rule as a state change is the removal alone, and the parent root with that
/// removal applied is the root the second call returns. Returns the account to remove (its code and
/// storage go with it); removing an absent account is a no-op, as `SelfDestruct` is.
pub fn tip_signing_wipe(spec: &ChainSpec, number: u64) -> Option<Address> {
    (spec.effective.tip_signing == ForkBlock::At(number)).then_some(BLOCK_SIGNERS)
}
