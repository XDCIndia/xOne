//! Block execution for xOne: one block on the state of its parent, in the order of geth-XDC
//! `StateProcessor.Process` (`core/state_processor.go:115-290`), and nothing copied from the header.
//! The commitments of the result (transactions root, receipts root, bloom, state root, gas used)
//! are computed here from the body and the executed state; the caller compares them with the
//! header. The order is written down in `spec/execution/block-processing-order.md`.
#![forbid(unsafe_code)]

mod overlay;
pub mod tx;

pub use overlay::OverlayError;
pub use tx::{Tx, TxError};

use alloy_primitives::{Address, Bloom, Bytes, B256};
use alloy_trie::root::ordered_trie_root_encoded;
use overlay::Overlay;
use revm::{
    context::{result::EVMError, ContextSetters, TxEnv},
    context_interface::{result::ExecutionResult, JournalTr},
    handler::Handler,
    primitives::TxKind,
};
use std::collections::BTreeMap;
use xone_chainspec::ChainSpec;
use xone_consensus::{
    chain::{self, BlockReader, SignTxCounter},
    reward::{self, v1 as reward_v1, Holders, RewardWindow, Rule},
    verify::{
        Body, BodyError, BodyVerifier, Context as VerifyContext, HeaderError, HeaderVerifier,
    },
};
use xone_evm::{
    basefee::{self, BaseFeeError},
    fees::block_author,
    handler::{build_evm_with_rules, XdcHandler},
    journal::account_true_balance,
    route::{route, Route},
    spec::{rules, SpecError, XdcBlock},
    special::{apply_empty_tx, apply_sign_tx, tip_signing_wipe, Receipt, SpecialError},
};
use xone_primitives::{Header, SealError, WideBalance};
use xone_state::{system_contracts::validator_0x88, Missing, StateDiff, StateRead};

/// geth `common.TRC21IssuerSMC` per chain (`common/types.go:124`, `:213`, `:229`): Apothem has its own,
/// every other chain the mainnet one. Its slot 1 holds the length of the token list.
fn trc21_issuer(chain_id: u64) -> Address {
    if chain_id == 51 {
        alloy_primitives::address!("0E2C88753131CE01c7551B726b28BFD04e44003F")
    } else {
        alloy_primitives::address!("8c0faeb5C6bEd2129b8674F262Fd45c4e9468bee")
    }
}

/// What block execution reads besides the parent state: the chain the block extends and the code
/// store. Every answer is about the canonical chain of the block being executed.
pub trait Env: BlockReader {
    /// The hash of canonical block `number` (BLOCKHASH).
    fn hash_of(&self, number: u64) -> Option<B256>;
    /// The masternodes listed in the `extraData` of the V1 checkpoint header `number`
    /// (geth `GetMasternodesFromCheckpointHeader`).
    fn masternodes_at(&self, checkpoint: u64) -> Option<Vec<Address>>;
    /// Every account whose balance is above 2^256, with its true balance, in the parent state.
    fn wide_balances(&self) -> Vec<(Address, WideBalance)>;
    /// Contract code by hash.
    fn code(&self, hash: B256) -> Option<Bytes>;
}

pub struct ExecCtx<'a, E> {
    pub parent: &'a Header,
    /// What the header verifier needs of the block's epoch (masternodes, snapshot signers).
    pub verify: &'a VerifyContext,
    pub env: &'a E,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("header: {0}")]
    Header(#[from] HeaderError),
    #[error("base fee: {0}")]
    BaseFee(#[from] BaseFeeError),
    #[error("seal: {0}")]
    Seal(#[from] SealError),
    #[error("rules: {0}")]
    Spec(#[from] SpecError),
    #[error("transaction {index}: {error}")]
    Tx { index: usize, error: TxError },
    #[error("transaction {index}: gas limit reached")]
    GasLimitReached { index: usize },
    #[error("transaction {index}: {reason}")]
    Execution { index: usize, reason: String },
    #[error("transaction {index}: {error}")]
    Special { index: usize, error: SpecialError },
    #[error("state: {0}")]
    State(#[from] OverlayError),
    #[error("reward: {0}")]
    Reward(#[from] chain::Error),
    #[error("state view: {0}")]
    View(#[from] Missing),
    #[error("not supported by the block executor: {0}")]
    Unsupported(&'static str),
}

/// The result of executing one block.
#[derive(Debug)]
pub struct BlockOutcome {
    /// Hash of the header as given (its fields are not changed by execution).
    pub hash: B256,
    pub diff: StateDiff,
    /// Contract code deployed by the block.
    pub codes: BTreeMap<B256, Bytes>,
    pub receipts: Vec<Receipt>,
    /// `Receipt.MarshalBinary` of each receipt.
    pub receipt_encodings: Vec<Bytes>,
    /// The sign transactions of the block in block order, with the status of their own receipt:
    /// what later checkpoints count (`chain::SignTx`).
    pub signs: Vec<chain::SignTx>,
    pub gas_used: u64,
    pub bloom: Bloom,
    pub state_root: B256,
    pub receipt_root: B256,
    pub tx_root: B256,
    /// geth `ValidateBody` and the receipt checks of `ValidateState`, run with the receipts and
    /// roots computed above. `Err` is a block geth would refuse; the caller reports the first
    /// differing field before it reports this.
    pub body_check: Result<(), BodyError>,
}

fn evm_error(index: usize, e: impl std::fmt::Display) -> ExecError {
    ExecError::Execution {
        index,
        reason: e.to_string(),
    }
}

/// Execute `header` and `body` on `parent_view`, the state after `ctx.parent`.
pub fn execute_block<S: StateRead, E: Env>(
    spec: &ChainSpec,
    parent_view: &S,
    header: &Header,
    body: &Body,
    ctx: &ExecCtx<'_, E>,
) -> Result<BlockOutcome, ExecError> {
    let number = header.number;
    // geth `engine.VerifyHeader` and `eip1559.VerifyEIP1559Header`: before any state is touched.
    HeaderVerifier::new(spec).verify(header, ctx.parent, u64::MAX, ctx.verify)?;
    basefee::verify(spec, number, header.base_fee)?;
    let r = rules(spec, number)?;
    // TRC21 token fees (`ApplyTRC21TokenFee`, `core/state_processor.go:254`) are XB-08. They apply to a
    // transaction sent to a token that the issuer contract lists (`GetTRC21FeeCapacityFromState`,
    // `core/state/trc21_reader.go:52-67`); with no token listed the capacity map is empty, no
    // transaction takes that path and `UpdateTRC21Fee` writes nothing (`:134`).
    if spec.effective.tip_trc21_fee.is_active(number)
        && !parent_view
            .storage(trc21_issuer(spec.chain_id), B256::with_last_byte(1))
            .is_zero()
    {
        return Err(ExecError::Unsupported(
            "TRC21 token fees (TIPTRC21Fee) with tokens registered at the issuer",
        ));
    }
    let eip158 = spec.eip158.is_active(number);
    let author = block_author(spec, header)?;
    let txs = body
        .transactions
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            tx::decode(raw, spec.chain_id).map_err(|error| ExecError::Tx { index, error })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut overlay = Overlay::new(parent_view, ctx.env);
    // `ApplyTIPSigningHardFork`, `core/state_processor.go:152`, `:321-327`: before the first transaction.
    if let Some(wiped) = tip_signing_wipe(spec, number) {
        overlay.delete(wiped);
    }
    // `getCoinbaseOwner`, `core/state_processor.go:103`, `:182`: the fee owner is read before the loop, from the parent state.
    let owner = if r.fee_to_owner {
        validator_0x88::candidate_owner(parent_view, author)
    } else {
        None
    };
    let block = XdcBlock {
        number,
        timestamp: header.timestamp,
        beneficiary: author,
        gas_limit: header.gas_limit,
        difficulty: header.difficulty,
        base_fee: header
            .base_fee
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ExecError::Unsupported("base fee above 2^64"))?,
    };
    let mut evm = build_evm_with_rules(overlay, spec, r.clone(), &block, owner);
    // geth-XDC `BigBalance` (`core/state/state_object.go`): the journal carries the true balance of
    // every wide account in a side map that lives for the whole block (`xone_evm::journal`).
    for (a, w) in ctx.env.wide_balances() {
        evm.ctx.journaled_state.side.insert(a, w);
    }

    let mut receipts: Vec<Receipt> = Vec::with_capacity(txs.len());
    let mut used = 0u64;
    for (index, t) in txs.iter().enumerate() {
        // `RouteTransaction`, `core/state_processor.go:856-872`, used at `:235-258`.
        match route(&r, t.to) {
            Route::SignTx => {
                let db = evm.ctx.journaled_state.db_and_state_mut().0;
                let state_nonce = db.account(t.from).map_or(0, |a| a.nonce);
                let (nonce, receipt) = apply_sign_tx(spec, number, state_nonce, t.nonce, used)
                    .map_err(|error| ExecError::Special { index, error })?;
                db.set_nonce(t.from, nonce)?;
                receipts.push(receipt);
            }
            Route::XdcxEmpty => {
                let to =
                    t.to.ok_or(ExecError::Unsupported("empty transaction without a target"))?;
                let receipt = apply_empty_tx(spec, number, to, used)
                    .map_err(|error| ExecError::Special { index, error })?;
                receipts.push(receipt);
            }
            Route::RandomizeTx | Route::Evm => {
                // `gp.SubGas(msg.GasLimit)`, `core/state_transition.go`: the pool must hold the whole limit
                if t.gas > header.gas_limit.saturating_sub(used) {
                    return Err(ExecError::GasLimitReached { index });
                }
                evm.ctx.set_tx(tx_env(spec, t));
                let result = XdcHandler::<Overlay<'_, S, E>>::default()
                    .run(&mut evm)
                    .map_err(|e| match e {
                        EVMError::Database(e) => ExecError::State(e),
                        other => evm_error(index, other),
                    })?;
                let (gas, success, logs) = match result {
                    ExecutionResult::Success { gas, logs, .. } => (gas.tx_gas_used(), true, logs),
                    ExecutionResult::Revert { gas, .. } | ExecutionResult::Halt { gas, .. } => {
                        (gas.tx_gas_used(), false, vec![])
                    }
                };
                let state = evm.ctx.journaled_state.finalize();
                let true_balances: BTreeMap<Address, WideBalance> = state
                    .iter()
                    .map(|(a, acct)| {
                        (
                            *a,
                            account_true_balance(acct, evm.ctx.journaled_state.side.get(a)),
                        )
                    })
                    .collect();
                let touched: Vec<Address> = state.keys().copied().collect();
                let db = evm.ctx.journaled_state.db_and_state_mut().0;
                db.commit(state, |a, _| true_balances[a].clone(), eip158)?;
                let gone: Vec<Address> = touched
                    .into_iter()
                    .filter(|a| db.account(*a).is_none())
                    .collect();
                for a in gone {
                    evm.ctx.journaled_state.side.forget(&a);
                }
                used += gas;
                receipts.push(Receipt::new(t.ty, success, used, gas, logs));
            }
        }
    }

    // `Finalize`, `consensus/XDPoS/xdpos.go:1498`, `:1593-1612`: at a V1 checkpoint the reward is
    // paid from the state read before the transactions.
    let db = evm.ctx.journaled_state.db_and_state_mut().0;
    let period = reward_v1::checkpoint_period(spec);
    if number.is_multiple_of(period) {
        // `eth/hooks/engine_v2_hooks.go:81`: `number > rCheckpoint`; the first checkpoint pays nothing.
        if number > period {
            if reward::rule(spec, number) != Rule::V1 {
                return Err(ExecError::Unsupported(
                    "checkpoint rewards of V2 and the tiered rules",
                ));
            }
            let at = reward_v1::locate(spec, number)?;
            let blocks =
                SignTxCounter::new(ctx.env).collect((header.parent_hash, number - 1), at.start)?;
            let masternodes =
                ctx.env
                    .masternodes_at(at.checkpoint)
                    .ok_or(chain::Error::Missing {
                        what: "checkpoint header",
                        number: at.checkpoint,
                        hash: B256::ZERO,
                    })?;
            let window = RewardWindow::new(at.start, at.end, blocks, masternodes, vec![], vec![])?;
            let holders = Holders::read(
                ctx.parent.state_root,
                parent_view,
                window.masternodes().iter().copied(),
            );
            for (holder, amount) in
                reward_v1::rewards(spec, number, &window, &holders, ctx.parent.state_root)?
            {
                db.credit(holder, amount, eip158)?;
            }
        }
    }

    let signs = txs
        .iter()
        .zip(&receipts)
        .filter_map(|(t, r)| {
            chain::signed_hash(t.to, &t.data).map(|signed| chain::SignTx {
                sender: t.from,
                signed,
                ok: r.success,
            })
        })
        .collect();
    let (diff, codes) = db.take();
    let tx_root = ordered_trie_root_encoded(&body.transactions);
    let receipt_encodings: Vec<Bytes> = receipts.iter().map(|x| Bytes::from(x.encode())).collect();
    let receipt_root = ordered_trie_root_encoded(&receipt_encodings);
    let bloom = receipts.iter().fold(Bloom::ZERO, |b, x| b | x.bloom);
    let state_root = parent_view.root_with(&diff)?;
    let body_check = BodyVerifier::new(spec).verify(header, body, &receipt_encodings);
    Ok(BlockOutcome {
        hash: header.hash(),
        diff,
        codes,
        receipts,
        receipt_encodings,
        signs,
        gas_used: used,
        bloom,
        state_root,
        receipt_root,
        tx_root,
        body_check,
    })
}

fn tx_env(spec: &ChainSpec, t: &Tx) -> TxEnv {
    TxEnv {
        tx_type: t.ty,
        caller: t.from,
        gas_limit: t.gas,
        gas_price: t.gas_price,
        gas_priority_fee: t.priority_fee,
        kind: t.to.map_or(TxKind::Create, TxKind::Call),
        value: t.value,
        data: t.data.clone(),
        nonce: t.nonce,
        chain_id: (t.ty >= 1).then_some(spec.chain_id),
        access_list: t
            .access_list
            .iter()
            .map(|i| revm::context::transaction::AccessListItem {
                address: i.address,
                storage_keys: i.keys.clone(),
            })
            .collect::<Vec<_>>()
            .into(),
        ..Default::default()
    }
}
