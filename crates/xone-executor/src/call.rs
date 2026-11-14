//! One message on the state after a block, for `eth_call` and `eth_estimateGas` (geth-XDC
//! `internal/ethapi/api.go` `doCall`/`applyMessage` and `eth/gasestimator/gasestimator.go`).
//!
//! The message runs on a throw-away [`Overlay`] over the given view: nothing it writes reaches the
//! view. The block context is the header of the selected block (number, time, gas limit,
//! difficulty, base fee, the sealer as coinbase), with geth's simulation switches:
//! `SkipNonceChecks`, `SkipTransactionChecks` (no EOA check), no block gas pool other than the
//! caller's cap, `SkipExecutionGasCapCheck`, and `NoBaseFee` (a zero gas price zeroes the base fee).

use std::sync::atomic::{AtomicBool, Ordering};

use alloy_primitives::{Address, Bytes, U256};
use revm::{
    context::{result::EVMError, ContextSetters, TxEnv},
    context_interface::result::{
        ExecutionResult, HaltReason, InvalidTransaction, OutOfGasError, Output,
    },
    handler::Handler,
    primitives::TxKind,
};
use xone_chainspec::ChainSpec;
use xone_evm::{
    fees::block_author,
    handler::{build_evm_with_rules, XdcHandler, XdcTxError},
    spec::{rules, XdcBlock},
};
use xone_primitives::Header;
use xone_state::{system_contracts::validator_0x88, StateRead};

use crate::{overlay::Overlay, Env};

/// geth `params.TxGas`.
const TX_GAS: u64 = 21_000;
/// geth `params.CallStipend`.
const CALL_STIPEND: u64 = 2_300;
/// geth `estimateGasErrorRatio` (`internal/ethapi/api.go`): 0.015, as a fraction.
const ERROR_RATIO_NUM: u128 = 15;
const ERROR_RATIO_DEN: u128 = 1000;

/// The message of an `eth_call` (geth `TransactionArgs.ToMessage` input).
#[derive(Debug, Clone, Default)]
pub struct CallMsg {
    pub from: Address,
    pub to: Option<Address>,
    pub gas: u64,
    pub gas_price: Option<U256>,
    pub max_fee: Option<U256>,
    pub max_priority_fee: Option<U256>,
    pub value: U256,
    pub data: Bytes,
}

/// How the message ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallStatus {
    Success,
    /// `REVERT`: the output holds the revert data.
    Revert,
    /// Any other VM error, with geth's `core/vm/errors.go` text.
    Halt(String),
}

#[derive(Debug, Clone)]
pub struct CallResult {
    pub output: Bytes,
    /// geth `ExecutionResult.UsedGas`: after refunds.
    pub gas_used: u64,
    /// geth `ExecutionResult.MaxUsedGas`: before refunds.
    pub max_used: u64,
    pub status: CallStatus,
}

impl CallResult {
    pub fn failed(&self) -> bool {
        self.status != CallStatus::Success
    }
}

/// A message geth refuses before it runs (`preCheck`, `buyGas`, intrinsic gas), with geth's text,
/// or a state read that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// Raise the gas limit (geth `ErrIntrinsicGas`, `ErrFloorDataGas`).
    #[error("{0}")]
    GasTooLow(String),
    /// Lower the gas limit (geth `ErrGasLimitTooHigh`).
    #[error("{0}")]
    GasTooHigh(String),
    #[error("{0}")]
    Invalid(String),
    #[error("state: {0}")]
    State(String),
}

/// geth `newRevertError` and `abi.UnpackRevert`: `execution reverted`, with the reason of an
/// `Error(string)` or `Panic(uint256)` payload.
pub fn revert_message(data: &[u8]) -> String {
    match revert_reason(data) {
        Some(r) => format!("execution reverted: {r}"),
        None => "execution reverted".to_owned(),
    }
}

fn revert_reason(data: &[u8]) -> Option<String> {
    let (selector, rest) = (data.get(..4)?, data.get(4..)?);
    let word = |i: usize| rest.get(i * 32..i * 32 + 32);
    let small = |w: &[u8]| -> Option<usize> { usize::try_from(U256::from_be_slice(w)).ok() };
    match selector {
        [0x08, 0xc3, 0x79, 0xa0] => {
            let off = small(word(0)?)?;
            let len_at = rest.get(off..off.checked_add(32)?)?;
            let len = small(len_at)?;
            let start = off.checked_add(32)?;
            let s = rest.get(start..start.checked_add(len)?)?;
            Some(String::from_utf8_lossy(s).into_owned())
        }
        [0x4e, 0x48, 0x7b, 0x71] => {
            let code = U256::from_be_slice(word(0)?);
            let known = match u64::try_from(code).unwrap_or(u64::MAX) {
                0x00 => "generic panic",
                0x01 => "assert(false)",
                0x11 => "arithmetic underflow or overflow",
                0x12 => "division or modulo by zero",
                0x21 => "enum overflow",
                0x22 => "invalid encoded storage byte array accessed",
                0x31 => "out-of-bounds array access; popping on an empty array",
                0x32 => "out-of-bounds access of an array or bytesSlice",
                0x41 => "out of memory",
                0x51 => "uninitialized function",
                _ => return Some(format!("unknown panic code: {code:#x}")),
            };
            Some(known.to_owned())
        }
        _ => None,
    }
}

/// geth `core/vm/errors.go` text of a halt.
fn halt_text(h: &HaltReason) -> String {
    match h {
        HaltReason::OutOfGas(OutOfGasError::InvalidOperand) => "gas uint64 overflow".to_owned(),
        HaltReason::OutOfGas(_) => "out of gas".to_owned(),
        HaltReason::OpcodeNotFound | HaltReason::NotActivated | HaltReason::InvalidFEOpcode => {
            "invalid opcode".to_owned()
        }
        HaltReason::InvalidJump => "invalid jump destination".to_owned(),
        HaltReason::StackUnderflow => "stack underflow".to_owned(),
        HaltReason::StackOverflow => "stack limit reached 1024".to_owned(),
        HaltReason::OutOfOffset => "return data out of bounds".to_owned(),
        HaltReason::CreateCollision => "contract address collision".to_owned(),
        HaltReason::NonceOverflow => "nonce uint64 overflow".to_owned(),
        HaltReason::CreateContractSizeLimit => "max code size exceeded".to_owned(),
        HaltReason::CreateContractStartingWithEF => {
            "invalid code: must not begin with 0xef".to_owned()
        }
        HaltReason::CreateInitCodeSizeLimit => "max initcode size exceeded".to_owned(),
        HaltReason::StateChangeDuringStaticCall | HaltReason::CallNotAllowedInsideStatic => {
            "write protection".to_owned()
        }
        HaltReason::OutOfFunds => "insufficient balance for transfer".to_owned(),
        HaltReason::CallTooDeep => "max call depth exceeded".to_owned(),
        HaltReason::PrecompileError | HaltReason::PrecompileErrorWithContext(_) => {
            "precompiled contract failed".to_owned()
        }
        other => other.to_string(),
    }
}

/// geth `core/error.go` text of a message refused before execution.
fn invalid(e: &InvalidTransaction, m: &CallMsg) -> CallError {
    match e {
        InvalidTransaction::LackOfFundForMaxFee { fee, balance } => CallError::Invalid(format!(
            "insufficient funds for gas * price + value: address {} have {balance} want {fee}",
            m.from
        )),
        InvalidTransaction::GasPriceLessThanBasefee => CallError::Invalid(format!(
            "max fee per gas less than block base fee: address {}",
            m.from
        )),
        InvalidTransaction::PriorityFeeGreaterThanMaxFee => CallError::Invalid(format!(
            "max priority fee per gas higher than max fee per gas: address {}",
            m.from
        )),
        InvalidTransaction::CallGasCostMoreThanGasLimit {
            initial_gas,
            gas_limit,
        } => CallError::GasTooLow(format!(
            "intrinsic gas too low: have {gas_limit}, want {initial_gas}"
        )),
        InvalidTransaction::GasFloorMoreThanGasLimit {
            gas_floor,
            gas_limit,
        } => CallError::GasTooLow(format!(
            "insufficient gas for floor data gas cost: have {gas_limit}, want {gas_floor}"
        )),
        InvalidTransaction::TxGasLimitGreaterThanCap { gas_limit, cap } => CallError::GasTooHigh(
            format!("transaction gas limit too high (cap: {cap}, tx: {gas_limit})"),
        ),
        other => CallError::Invalid(other.to_string()),
    }
}

/// The effective gas price of `m` under `base_fee` (geth `ToMessage`): the legacy price, or
/// `min(tip + baseFee, feeCap)`; zero when no price field is set.
fn price_of(m: &CallMsg, base_fee: Option<U256>) -> U256 {
    match (base_fee, m.gas_price) {
        (_, Some(p)) => p,
        (None, None) => U256::ZERO,
        (Some(b), None) => {
            let cap = m.max_fee.unwrap_or(U256::ZERO);
            let tip = m.max_priority_fee.unwrap_or(U256::ZERO);
            if cap.is_zero() && tip.is_zero() {
                U256::ZERO
            } else {
                cap.min(tip.saturating_add(b))
            }
        }
    }
}

fn tx_env(spec: &ChainSpec, m: &CallMsg, base_fee: Option<U256>) -> Result<TxEnv, CallError> {
    let narrow = |v: U256| {
        u128::try_from(v).map_err(|_| CallError::Invalid("gas price above 2^128".to_owned()))
    };
    let dynamic = base_fee.is_some() && m.gas_price.is_none();
    let (tx_type, gas_price, gas_priority_fee) = if dynamic {
        let cap = m.max_fee.unwrap_or(U256::ZERO);
        let tip = m.max_priority_fee.unwrap_or(U256::ZERO);
        (2u8, narrow(cap)?, Some(narrow(tip)?))
    } else {
        (0u8, narrow(price_of(m, base_fee))?, None)
    };
    Ok(TxEnv {
        tx_type,
        caller: m.from,
        gas_limit: m.gas,
        gas_price,
        gas_priority_fee,
        kind: m.to.map_or(TxKind::Create, TxKind::Call),
        value: m.value,
        data: m.data.clone(),
        nonce: 0,
        chain_id: (tx_type >= 1).then_some(spec.chain_id),
        ..Default::default()
    })
}

/// Runs `m` on `view`, the state after `header`, with `header` as the block context (geth
/// `doCall`). The view is not changed.
pub fn call<V: StateRead, E: Env>(
    spec: &ChainSpec,
    view: &V,
    header: &Header,
    env: &E,
    m: &CallMsg,
) -> Result<CallResult, CallError> {
    let r = rules(spec, header.number).map_err(|e| CallError::Invalid(e.to_string()))?;
    // geth `NewEVMBlockContext`: the coinbase is the engine's author of the header.
    let author = match block_author(spec, header) {
        Ok(a) => a,
        Err(_) => header.coinbase,
    };
    let owner = if r.fee_to_owner {
        validator_0x88::candidate_owner(view, author)
    } else {
        None
    };
    let price = price_of(m, header.base_fee);
    let block = XdcBlock {
        number: header.number,
        timestamp: header.timestamp,
        beneficiary: author,
        gas_limit: header.gas_limit,
        difficulty: header.difficulty,
        base_fee: header
            .base_fee
            .map(u64::try_from)
            .transpose()
            .map_err(|_| CallError::Invalid("base fee above 2^64".to_owned()))?,
    };
    let header_fee = r.header_has_basefee;
    let overlay = Overlay::new(view, env);
    let mut evm = build_evm_with_rules(overlay, spec, r, &block, owner);
    // geth `SkipNonceChecks`, `SkipTransactionChecks`, the caller's gas pool instead of the
    // block's, `SkipExecutionGasCapCheck` (EIP-7825 only: from EIP-8037 the cap splits the gas).
    evm.ctx.cfg.disable_nonce_check = true;
    evm.ctx.cfg.disable_eip3607 = true;
    evm.ctx.cfg.disable_block_gas_limit = true;
    if !revm::context_interface::Cfg::is_amsterdam_eip8037_enabled(&evm.ctx.cfg) {
        evm.ctx.cfg.tx_gas_limit_cap = Some(u64::MAX);
    }
    // geth `applyMessage`: "Lower the basefee to 0 to avoid breaking EVM invariants".
    if price.is_zero() && header_fee {
        evm.ctx.block.basefee = 0;
    }
    for (a, w) in env.wide_balances() {
        evm.ctx.journaled_state.side.insert(a, w);
    }
    evm.ctx
        .set_tx(tx_env(spec, m, header.base_fee.filter(|_| header_fee))?);
    let result = XdcHandler::<Overlay<'_, V, E>>::default()
        .run(&mut evm)
        .map_err(|e| match e {
            EVMError::Transaction(XdcTxError::Invalid(t)) => invalid(&t, m),
            EVMError::Database(e) => CallError::State(e.to_string()),
            other => CallError::Invalid(other.to_string()),
        })?;
    Ok(match result {
        ExecutionResult::Success { gas, output, .. } => CallResult {
            output: match output {
                Output::Call(b) => b,
                Output::Create(b, _) => b,
            },
            gas_used: gas.tx_gas_used(),
            max_used: gas.total_gas_spent(),
            status: CallStatus::Success,
        },
        ExecutionResult::Revert { gas, output, .. } => CallResult {
            output,
            gas_used: gas.tx_gas_used(),
            max_used: gas.total_gas_spent(),
            status: CallStatus::Revert,
        },
        ExecutionResult::Halt { gas, reason, .. } => CallResult {
            output: Bytes::new(),
            gas_used: gas.tx_gas_used(),
            max_used: gas.total_gas_spent(),
            status: CallStatus::Halt(halt_text(&reason)),
        },
    })
}

/// Why an estimate has no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EstimateError {
    /// The message reverts at the highest allowed gas: the revert data.
    #[error("execution reverted")]
    Reverted(Bytes),
    #[error("{0}")]
    Failed(String),
}

/// geth `gasestimator.Estimate`: the lowest gas limit, within 1.5 %, at which `m` succeeds on
/// `view`. `m.gas` below 21000 means "not given" (the header's gas limit is the ceiling);
/// `gas_cap` 0 means no cap. `cancel` is read between runs.
pub fn estimate_gas<V: StateRead, E: Env>(
    spec: &ChainSpec,
    view: &V,
    header: &Header,
    env: &E,
    m: &CallMsg,
    gas_cap: u64,
    cancel: &AtomicBool,
) -> Result<u64, EstimateError> {
    let mut hi = if m.gas >= TX_GAS {
        m.gas
    } else {
        header.gas_limit
    };
    if spec.osaka.is_active(header.number) && !spec.amsterdam.is_active(header.number) {
        hi = hi.min(1 << 24);
    }
    let fee_cap = m.max_fee.or(m.gas_price).unwrap_or(U256::ZERO);
    if !fee_cap.is_zero() {
        let balance = view.account(m.from).map_or(U256::ZERO, |a| a.balance.low());
        let wide = view.account(m.from).is_some_and(|a| a.balance.is_wide());
        if !wide {
            if m.value >= balance && !m.value.is_zero() {
                return Err(EstimateError::Failed(
                    "insufficient funds for transfer".to_owned(),
                ));
            }
            let allowance = (balance - m.value.min(balance)) / fee_cap;
            if let Ok(a) = u64::try_from(allowance) {
                hi = hi.min(a);
            }
        }
    }
    if gas_cap != 0 {
        hi = hi.min(gas_cap);
    }
    let run = |gas: u64| -> Result<(bool, Option<CallResult>), EstimateError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(EstimateError::Failed("request cancelled".to_owned()));
        }
        let msg = CallMsg { gas, ..m.clone() };
        match call(spec, view, header, env, &msg) {
            Ok(res) => Ok((res.failed(), Some(res))),
            Err(CallError::GasTooLow(_) | CallError::GasTooHigh(_)) => Ok((true, None)),
            Err(e) => Err(EstimateError::Failed(format!("failed with {gas} gas: {e}"))),
        }
    };
    // a plain transfer to an account without code costs exactly 21000
    if m.data.is_empty() {
        if let Some(to) = m.to {
            let no_code = view
                .account(to)
                .is_none_or(|a| a.code_hash == revm::primitives::KECCAK_EMPTY);
            if no_code {
                if let (false, Some(res)) = run(TX_GAS)? {
                    return Ok(res.gas_used);
                }
            }
        }
    }
    let (failed, res) = run(hi)?;
    let res = match (failed, res) {
        (false, Some(res)) => res,
        (_, Some(res)) if res.status == CallStatus::Revert => {
            return Err(EstimateError::Reverted(res.output))
        }
        (
            _,
            Some(CallResult {
                status: CallStatus::Halt(h),
                ..
            }),
        ) if h != "out of gas" => return Err(EstimateError::Failed(h)),
        _ => {
            return Err(EstimateError::Failed(format!(
                "gas required exceeds allowance ({hi})"
            )))
        }
    };
    let mut lo = res.gas_used.saturating_sub(1);
    let optimistic = (res.max_used + CALL_STIPEND) * 64 / 63;
    if optimistic < hi {
        if run(optimistic)?.0 {
            lo = optimistic;
        } else {
            hi = optimistic;
        }
    }
    while lo + 1 < hi {
        // (hi - lo) / hi < 1.5 %
        if u128::from(hi - lo) * ERROR_RATIO_DEN < u128::from(hi) * ERROR_RATIO_NUM {
            break;
        }
        let mid = (lo + (hi - lo) / 2).min(lo.saturating_mul(2));
        if run(mid)?.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, keccak256, B256};
    use std::collections::BTreeMap;
    use xone_consensus::chain::{BlockReader, HeaderView, SignTx};
    use xone_primitives::WideBalance;
    use xone_state::Account;

    const STORE: Address = address!("00000000000000000000000000000000000000aa");
    const REVERT: Address = address!("00000000000000000000000000000000000000bb");
    const RICH: Address = address!("00000000000000000000000000000000000000cc");

    #[derive(Default)]
    struct Mem {
        accounts: BTreeMap<Address, Account>,
        codes: BTreeMap<B256, Bytes>,
    }

    impl Mem {
        fn contract(&mut self, a: Address, code: &[u8]) {
            let h = keccak256(code);
            self.codes.insert(h, Bytes::copy_from_slice(code));
            self.accounts.insert(
                a,
                Account {
                    code_hash: h,
                    ..Account::default()
                },
            );
        }
    }

    impl StateRead for Mem {
        fn account(&self, a: Address) -> Option<Account> {
            self.accounts.get(&a).cloned()
        }
        fn storage(&self, _: Address, _: B256) -> U256 {
            U256::ZERO
        }
    }
    impl BlockReader for Mem {
        fn header(&self, _: B256, _: u64) -> Option<HeaderView> {
            None
        }
        fn sign_txs(&self, _: B256, _: u64) -> Option<Vec<SignTx>> {
            None
        }
    }
    impl Env for Mem {
        fn hash_of(&self, _: u64) -> Option<B256> {
            None
        }
        fn masternodes_at(&self, _: u64) -> Option<Vec<Address>> {
            None
        }
        fn wide_balances(&self) -> Vec<(Address, WideBalance)> {
            vec![]
        }
        fn code(&self, h: B256) -> Option<Bytes> {
            self.codes.get(&h).cloned()
        }
        fn standby_pool(&self, _: u64) -> Option<Vec<Address>> {
            None
        }
        fn burn(&self, _: u64) -> Option<U256> {
            None
        }
    }

    fn setup() -> (ChainSpec, Mem, Header) {
        let mut m = Mem::default();
        // SSTORE(0, 42); MSTORE(0, SLOAD(0)); RETURN(0, 32)
        m.contract(
            STORE,
            &[
                0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60,
                0x00, 0xf3,
            ],
        );
        // REVERT(0, 0)
        m.contract(REVERT, &[0x60, 0x00, 0x60, 0x00, 0xfd]);
        m.accounts.insert(
            RICH,
            Account {
                balance: WideBalance::from(U256::from(10u64).pow(U256::from(18u64))),
                ..Account::default()
            },
        );
        let header = Header {
            number: 1_000,
            gas_limit: 50_000_000,
            timestamp: 1_600_000_000,
            ..Header::default()
        };
        (xone_chainspec::apothem(), m, header)
    }

    fn msg(to: Address) -> CallMsg {
        CallMsg {
            to: Some(to),
            gas: 50_000_000,
            ..CallMsg::default()
        }
    }

    #[test]
    fn a_call_returns_the_output_and_leaves_the_view_alone() {
        let (spec, m, h) = setup();
        let r = call(&spec, &m, &h, &m, &msg(STORE)).unwrap();
        assert_eq!(r.status, CallStatus::Success);
        assert_eq!(U256::from_be_slice(&r.output), U256::from(42u64));
        assert!(r.gas_used > 21_000 + 20_000);
        // the write went to the overlay only
        assert_eq!(m.storage(STORE, B256::ZERO), U256::ZERO);
        assert_eq!(m.account(Address::ZERO), None);
    }

    #[test]
    fn a_call_without_funds_or_nonce_runs_and_a_priced_one_needs_the_balance() {
        let (spec, m, h) = setup();
        // the zero address, unknown and without balance: no nonce, EOA or fee check at price 0
        let r = call(&spec, &m, &h, &m, &msg(STORE)).unwrap();
        assert!(!r.failed());
        let priced = CallMsg {
            gas_price: Some(U256::from(1u64)),
            from: address!("00000000000000000000000000000000000000dd"),
            ..msg(STORE)
        };
        let e = call(&spec, &m, &h, &m, &priced).unwrap_err();
        assert!(
            e.to_string()
                .starts_with("insufficient funds for gas * price + value: address 0x"),
            "{e}"
        );
    }

    #[test]
    fn a_revert_and_too_little_gas_are_reported_as_geth_does() {
        let (spec, m, h) = setup();
        let r = call(&spec, &m, &h, &m, &msg(REVERT)).unwrap();
        assert_eq!(r.status, CallStatus::Revert);
        let low = CallMsg {
            gas: 20_000,
            ..msg(STORE)
        };
        let e = call(&spec, &m, &h, &m, &low).unwrap_err();
        assert_eq!(
            e.to_string(),
            "intrinsic gas too low: have 20000, want 21000"
        );
        let oog = CallMsg {
            gas: 30_000,
            ..msg(STORE)
        };
        let r = call(&spec, &m, &h, &m, &oog).unwrap();
        assert_eq!(r.status, CallStatus::Halt("out of gas".to_owned()));
    }

    #[test]
    fn estimate_finds_a_limit_that_succeeds() {
        let (spec, m, h) = setup();
        let no = AtomicBool::new(false);
        let at = |m2: &CallMsg| estimate_gas(&spec, &m, &h, &m, m2, 50_000_000, &no);
        let used = call(&spec, &m, &h, &m, &msg(STORE)).unwrap().gas_used;
        let est = at(&CallMsg {
            gas: 0,
            ..msg(STORE)
        })
        .unwrap();
        assert!(est >= used && est <= used + used / 50, "{est} vs {used}");
        let ok = call(
            &spec,
            &m,
            &h,
            &m,
            &CallMsg {
                gas: est,
                ..msg(STORE)
            },
        )
        .unwrap();
        assert!(!ok.failed());
        // a plain transfer to an account without code
        let plain = CallMsg {
            from: RICH,
            value: U256::from(1u64),
            gas: 0,
            ..msg(address!("00000000000000000000000000000000000000ee"))
        };
        assert_eq!(at(&plain).unwrap(), 21_000);
        assert_eq!(
            at(&CallMsg {
                gas: 0,
                ..msg(REVERT)
            })
            .unwrap_err(),
            EstimateError::Reverted(Bytes::new())
        );
        let cancelled = AtomicBool::new(true);
        assert!(estimate_gas(&spec, &m, &h, &m, &msg(STORE), 0, &cancelled).is_err());
    }

    #[test]
    fn revert_reasons_unpack_error_and_panic() {
        let mut e = vec![0x08, 0xc3, 0x79, 0xa0];
        e.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
        e.extend_from_slice(&U256::from(2u64).to_be_bytes::<32>());
        let mut s = [0u8; 32];
        s[..2].copy_from_slice(b"no");
        e.extend_from_slice(&s);
        assert_eq!(revert_message(&e), "execution reverted: no");
        let mut p = vec![0x4e, 0x48, 0x7b, 0x71];
        p.extend_from_slice(&U256::from(0x11u64).to_be_bytes::<32>());
        assert_eq!(
            revert_message(&p),
            "execution reverted: arithmetic underflow or overflow"
        );
        assert_eq!(revert_message(&[]), "execution reverted");
    }
}
