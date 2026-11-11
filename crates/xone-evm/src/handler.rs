//! The revm `Handler` and instruction overrides that carry the XDC couplings.
//!
//! Stock revm runs unchanged except where geth-XDC differs:
//!
//! * `validate_initial_tx_gas`: the EIP-7623 floor does not follow the calldata price;
//! * `validate_against_state_and_deduct_caller`: geth `buyGas` for a wide sender
//!   (`core/state_transition.go:497-557`);
//! * `run_exec_loop`: STATICCALL touches the callee only from the Istanbul rules
//!   (`core/vm/evm.go:513`);
//! * `reimburse_caller`, `reward_beneficiary`: a credit above the legal balance bound is an error,
//!   and the fee goes to the coinbase or, after `TIPTRC21Fee`, to the coinbase owner
//!   (`core/state_transition.go:881-930`);
//! * the BASEFEE, PREVRANDAO and PUSH0 instructions while `spec_id` is below London
//!   (`core/vm/jump_table.go:81-132`).

use crate::{
    gas,
    journal::WideJournal,
    precompiles::XdcPrecompiles,
    spec::{rules, SpecError, XdcBlock, XdcRules},
};
use alloy_primitives::{Address, U256};
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
        instructions::EthInstructions, pre_execution, EthFrame, EvmTr, FrameResult, Handler,
        ItemOrResult,
    },
    interpreter::{
        interpreter::EthInterpreter, interpreter_action::FrameInit, CallInputs, CallScheme,
        CallValue, FrameInput, Host, InitialAndFloorGas, InstructionContext, InstructionExecResult,
        InstructionResult, InterpreterTypes,
    },
    primitives::hardfork::SpecId,
};
use std::marker::PhantomData;
use xone_chainspec::ChainSpec;
use xone_primitives::WideBalanceError;

/// The `CHAIN` of the revm `Context`: what the handler needs besides revm's own environment.
#[derive(Debug, Clone)]
pub struct XdcChain {
    pub rules: XdcRules,
    /// Coinbase owner of the block (geth `GetCandidateOwner`); used when `rules.fee_to_owner`.
    /// `None` credits nothing, as geth does with no registered owner. Its derivation from state
    /// belongs to XB-08.
    pub fee_owner: Option<Address>,
}

pub type XdcCtx<DB> = Context<BlockEnv, TxEnv, CfgEnv, DB, WideJournal<DB>, XdcChain>;
pub type XdcEvm<DB> = Evm<
    XdcCtx<DB>,
    (),
    EthInstructions<EthInterpreter, XdcCtx<DB>>,
    XdcPrecompiles,
    EthFrame<EthInterpreter>,
>;

/// Failures that are not stock revm transaction errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XdcTxError {
    #[error(transparent)]
    Invalid(#[from] InvalidTransaction),
    #[error("balance credit above the legal bound: {0}")]
    WideBalance(#[from] WideBalanceError),
}

pub type XdcError<DB> = EVMError<<DB as Database>::Error, XdcTxError>;

fn wide<DB: Database>(e: WideBalanceError) -> XdcError<DB> {
    EVMError::Transaction(XdcTxError::WideBalance(e))
}

/// geth `core/vm/jump_table.go:94-130` and `core/vm/instructions.go:225`: PUSH0.
fn push0<IT: InterpreterTypes, H: Host + ?Sized>(
    cx: InstructionContext<'_, H, IT>,
) -> InstructionExecResult {
    use revm::interpreter::interpreter_types::StackTr;
    if !cx.interpreter.stack.push(U256::ZERO) {
        return Err(InstructionResult::StackOverflow);
    }
    Ok(())
}

/// BASEFEE: `BlockEnv::basefee` already holds what geth reports, see [`XdcRules::block_env`].
fn basefee<IT: InterpreterTypes, H: Host + ?Sized>(
    cx: InstructionContext<'_, H, IT>,
) -> InstructionExecResult {
    use revm::interpreter::interpreter_types::StackTr;
    if !cx.interpreter.stack.push(cx.host.basefee()) {
        return Err(InstructionResult::StackOverflow);
    }
    Ok(())
}

/// Opcode 0x44 as PREVRANDAO: `keccak256(block number)`, set in `BlockEnv::prevrandao`.
fn prevrandao<IT: InterpreterTypes, H: Host + ?Sized>(
    cx: InstructionContext<'_, H, IT>,
) -> InstructionExecResult {
    use revm::interpreter::interpreter_types::StackTr;
    let v = cx
        .host
        .prevrandao()
        .ok_or(InstructionResult::FatalExternalError)?;
    if !cx.interpreter.stack.push(v) {
        return Err(InstructionResult::StackOverflow);
    }
    Ok(())
}

/// Instruction table of the revm fork with the marker opcodes added when the rules ask for them.
pub fn instructions<DB: Database>(r: &XdcRules) -> EthInstructions<EthInterpreter, XdcCtx<DB>> {
    use revm::bytecode::opcode::{BASEFEE, DIFFICULTY, PUSH0};
    let mut i = EthInstructions::new_mainnet_with_spec(r.spec_id);
    if r.marker_opcodes {
        i.insert_instruction(PUSH0, revm::interpreter::Instruction::new(push0), 2);
        i.insert_instruction(BASEFEE, revm::interpreter::Instruction::new(basefee), 2);
        i.insert_instruction(
            DIFFICULTY,
            revm::interpreter::Instruction::new(prevrandao),
            2,
        );
    }
    i
}

/// An EVM for `block` of `spec` over `db`.
pub fn build_evm<DB: Database>(
    db: DB,
    spec: &ChainSpec,
    block: &XdcBlock,
    fee_owner: Option<Address>,
) -> Result<XdcEvm<DB>, SpecError> {
    let r = rules(spec, block.number)?;
    Ok(build_evm_with_rules(db, spec, r, block, fee_owner))
}

/// As [`build_evm`] with the rules given. The conformance sweep uses it to run a transaction under
/// a deliberately wrong rule and check that its named regression then fails.
pub fn build_evm_with_rules<DB: Database>(
    db: DB,
    spec: &ChainSpec,
    r: XdcRules,
    block: &XdcBlock,
    fee_owner: Option<Address>,
) -> XdcEvm<DB> {
    let mut ctx: Context<BlockEnv, TxEnv, CfgEnv, DB, WideJournal<DB>, ()> =
        Context::new(db, r.spec_id);
    ctx.cfg = r.cfg();
    ctx.block = r.block_env(block);
    let precompiles = XdcPrecompiles::new(&crate::precompiles::row(spec, &r, block.number));
    let instr = instructions::<DB>(&r);
    let ctx = ctx.with_chain(XdcChain {
        rules: r,
        fee_owner,
    });
    Evm::new(ctx, instr, precompiles)
}

pub struct XdcHandler<DB>(PhantomData<DB>);

impl<DB> Default for XdcHandler<DB> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<DB: Database> Handler for XdcHandler<DB> {
    type Evm = XdcEvm<DB>;
    type Error = XdcError<DB>;
    type HaltReason = HaltReason;

    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<InitialAndFloorGas, Self::Error> {
        let ctx = evm.ctx_ref();
        let (tx, r) = (ctx.tx(), &ctx.chain().rules);
        let mut g = ctx.cfg().gas_params().initial_tx_gas_for_tx(tx, None);
        g.set_floor_gas(
            gas::floor_params(r)
                .initial_tx_gas_for_tx(tx, None)
                .floor_gas(),
        );
        if g.initial_total_gas() > tx.gas_limit() {
            return Err(InvalidTransaction::CallGasCostMoreThanGasLimit {
                gas_limit: tx.gas_limit(),
                initial_gas: g.initial_total_gas(),
            }
            .into());
        }
        if r.spec_id >= SpecId::PRAGUE && g.floor_gas() > tx.gas_limit() {
            return Err(InvalidTransaction::GasFloorMoreThanGasLimit {
                gas_floor: g.floor_gas(),
                gas_limit: tx.gas_limit(),
            }
            .into());
        }
        Ok(g)
    }

    fn validate_against_state_and_deduct_caller(
        &self,
        evm: &mut Self::Evm,
        _: &mut InitialAndFloorGas,
    ) -> Result<(), Self::Error> {
        let (block, tx, cfg, journal, _, _) = evm.ctx().all_mut();
        let caller = tx.caller();
        // Narrow sender: stock revm, with a fast path in the journal.
        if !journal.true_balance(caller)?.is_wide() {
            return pre_execution::validate_against_state_and_deduct_caller(evm.ctx());
        }
        // Wide sender: geth `buyGas` skips the wrapped pre-check and subtracts the gas cost from
        // the true balance; value affordability is `CanTransfer` later (wide: always true).
        let mut acc = journal.load_account_with_code_mut(caller)?.data;
        pre_execution::validate_account_nonce_and_code_with_components(
            &acc.account().info,
            tx,
            cfg,
        )?;
        if !cfg.is_fee_charge_disabled() {
            // a transaction without blobs has no blob gas price: zero, as in revm
            #[allow(clippy::disallowed_methods)]
            let blob_price = block.blob_gasprice().unwrap_or_default();
            let spend = tx.effective_balance_spending(u128::from(block.basefee()), blob_price)?
                - tx.value();
            acc.decr_balance(spend);
        }
        if tx.kind().is_call() {
            acc.bump_nonce();
        }
        Ok(())
    }

    fn run_exec_loop(
        &mut self,
        evm: &mut Self::Evm,
        first: FrameInit,
    ) -> Result<FrameResult, Self::Error> {
        let res = evm.frame_init(first)?;
        if let ItemOrResult::Result(r) = res {
            return Ok(r);
        }
        loop {
            let result = match evm.frame_run()? {
                ItemOrResult::Item(mut init) => {
                    if !evm.ctx().chain().rules.static_call_touch {
                        skip_static_call_touch(&mut init);
                    }
                    match evm.frame_init(init)? {
                        ItemOrResult::Item(_) => continue,
                        ItemOrResult::Result(r) => r,
                    }
                }
                ItemOrResult::Result(r) => r,
            };
            if let Some(r) = evm.frame_return_result(result)? {
                return Ok(r);
            }
        }
    }

    fn reimburse_caller(
        &self,
        evm: &mut Self::Evm,
        exec: &mut FrameResult,
    ) -> Result<(), Self::Error> {
        let ctx = evm.ctx();
        if ctx.cfg().is_fee_charge_disabled() {
            return Ok(());
        }
        let g = exec.gas();
        let caller = ctx.tx().caller();
        let price = ctx
            .tx()
            .effective_gas_price(u128::from(ctx.block().basefee()));
        // `refunded()` is an i64 counter; the cast keeps revm's reimbursement arithmetic bit for bit
        #[allow(clippy::as_conversions)]
        let refunded = g.refunded() as u64;
        let unused = g.remaining() + g.reservoir() + refunded;
        let amount = U256::from(price.saturating_mul(u128::from(unused)));
        if !ctx
            .journal_mut()
            .load_account_mut(caller)?
            .data
            .incr_balance(amount)
        {
            return Err(wide::<DB>(WideBalanceError::AboveBound));
        }
        Ok(())
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec: &mut FrameResult,
    ) -> Result<(), Self::Error> {
        let (block, tx, cfg, journal, chain, _) = evm.ctx().all_mut();
        if cfg.is_fee_charge_disabled() {
            return Ok(());
        }
        let g = exec.gas();
        let used = u128::from(g.used().saturating_sub(g.reservoir()));
        let basefee = u128::from(block.basefee());
        let price = tx.effective_gas_price(basefee);
        // geth `core/state_transition.go:881-930`.
        let (to, per_gas) = if chain.rules.fee_to_owner {
            (chain.fee_owner, price)
        } else {
            let header_basefee = if chain.rules.header_has_basefee {
                basefee
            } else {
                0
            };
            (
                Some(block.beneficiary()),
                price.saturating_sub(header_basefee),
            )
        };
        if let Some(to) = to {
            if !journal
                .load_account_mut(to)?
                .data
                .incr_balance(U256::from(per_gas.saturating_mul(used)))
            {
                return Err(wide::<DB>(WideBalanceError::AboveBound));
            }
        }
        // A credit above the bound inside the frames (SELFDESTRUCT, transfers) was recorded.
        match journal.fault.take() {
            Some(e) => Err(wide::<DB>(e)),
            None => Ok(()),
        }
    }
}

/// A STATICCALL frame without the zero-value transfer: revm touches the callee in the transfer,
/// geth only from the Istanbul rules. `CallValue::Apparent(0)` skips the transfer and still hands
/// the callee a call value of zero.
fn skip_static_call_touch(init: &mut FrameInit) {
    if let FrameInput::Call(inputs) = &mut init.frame_input {
        let inputs: &mut CallInputs = inputs;
        if inputs.scheme == CallScheme::StaticCall {
            inputs.value = CallValue::Apparent(U256::ZERO);
        }
    }
}
