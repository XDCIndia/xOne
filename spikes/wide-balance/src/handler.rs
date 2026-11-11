//! revm handler for geth-XDC: only `validate_against_state_and_deduct_caller` differs.
//!
//! geth `buyGas` skips the wrapped pre-check for a wide sender and subtracts the gas cost
//! from the true balance; value affordability is `CanTransfer` later (wide: always true).

use crate::journal::WideJournal;
use revm::{
    context::{
        result::{EVMError, HaltReason, InvalidTransaction},
        BlockEnv, CfgEnv, Context, Evm, TxEnv,
    },
    context_interface::{
        journaled_state::account::JournaledAccountTr, Block, Cfg, ContextTr, Database, JournalTr,
        Transaction,
    },
    handler::{
        instructions::EthInstructions, pre_execution, EthFrame, EthPrecompiles, EvmTr, Handler,
    },
    interpreter::{interpreter::EthInterpreter, InitialAndFloorGas},
};
use std::marker::PhantomData;

pub type Ctx<DB> = Context<BlockEnv, TxEnv, CfgEnv, DB, WideJournal<DB>, ()>;
pub type WideEvm<DB> = Evm<
    Ctx<DB>,
    (),
    EthInstructions<EthInterpreter, Ctx<DB>>,
    EthPrecompiles,
    EthFrame<EthInterpreter>,
>;

pub struct WideHandler<DB>(PhantomData<DB>);

impl<DB> Default for WideHandler<DB> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<DB: Database> Handler for WideHandler<DB> {
    type Evm = WideEvm<DB>;
    type Error = EVMError<DB::Error, InvalidTransaction>;
    type HaltReason = HaltReason;

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
        _: &mut InitialAndFloorGas,
    ) -> Result<(), Self::Error> {
        let (block, tx, cfg, journal, _, _) = evm.ctx().all_mut();
        let caller = tx.caller();
        if !journal.true_balance(caller)?.is_wide() {
            return pre_execution::validate_against_state_and_deduct_caller(evm.ctx());
        }
        let mut acc = journal.load_account_with_code_mut(caller)?.data;
        pre_execution::validate_account_nonce_and_code_with_components(
            &acc.account().info,
            tx,
            cfg,
        )?;
        if !cfg.is_fee_charge_disabled() {
            let spend = tx.effective_balance_spending(
                block.basefee() as u128,
                block.blob_gasprice().unwrap_or_default(),
            )? - tx.value();
            acc.decr_balance(spend);
        }
        if tx.kind().is_call() {
            acc.bump_nonce();
        }
        Ok(())
    }
}
