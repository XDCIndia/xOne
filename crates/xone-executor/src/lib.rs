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

use alloy_primitives::{address, Address, Bloom, Bytes, B256, U256};
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
    reward::{self, tiered, v1 as reward_v1, v2 as reward_v2, Holders, RewardWindow, Rule},
    v2::epoch_switch,
    verify::{
        Body, BodyError, BodyVerifier, Context as VerifyContext, Derive, HeaderError,
        HeaderVerifier,
    },
};
use xone_evm::{
    basefee::{self, BaseFeeError},
    fees::{self, block_author},
    handler::{build_evm_with_rules, XdcHandler},
    journal::account_true_balance,
    route::{route, Route},
    spec::{rules, SpecError, XdcBlock},
    special::{apply_empty_tx, apply_sign_tx, tip_signing_wipe, Receipt, SpecialError},
};
use xone_primitives::{Header, SealError, WideBalance};
use xone_state::{
    system_contracts::{minted_record_0x9a as minted, trc21_issuer, validator_0x88},
    Missing, StateDiff, StateRead,
};

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
    /// geth `GetStandbynodes` (`engine_v2/engine.go:3351-3375`) for the epoch-switch header
    /// `switch`: the candidates of the epoch's gap snapshot minus its masternodes and penalties.
    /// `None` when the snapshot is not held.
    fn standby_pool(&self, switch: u64) -> Option<Vec<Address>>;
    /// `baseFee * gasUsed` of canonical block `number`, zero when its header has no base fee (the
    /// burnt amount the tiered reward records, `engine_v2_hooks.go:967-969`).
    fn burn(&self, number: u64) -> Option<U256>;
}

pub struct ExecCtx<'a, E> {
    pub parent: &'a Header,
    /// What the header verifier needs of the block's epoch (masternodes, snapshot signers).
    pub verify: &'a VerifyContext,
    pub env: &'a E,
    /// V2 only: computes the `Validators` and `Penalties` an epoch-switch header must carry, from
    /// the gap snapshot and the penalty engine. With it a V2 header goes through
    /// `HeaderVerifier::verify_derived`; without it `verify` compares with the lists in `verify`.
    pub derive: Option<Derive<'a>>,
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
    #[error("v2: {0}")]
    V2(#[from] xone_consensus::v2::Error),
    #[error("the extra data of block {0} has no V2 fields")]
    NoV2Fields(u64),
    #[error("the standby pool of epoch switch {0} is not available")]
    NoStandby(u64),
    #[error("the burnt fees of block {0} are not available")]
    NoBurn(u64),
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
pub fn execute_block<V: StateRead, E: Env>(
    spec: &ChainSpec,
    parent_view: &V,
    header: &Header,
    body: &Body,
    ctx: &ExecCtx<'_, E>,
) -> Result<BlockOutcome, ExecError> {
    // geth `engine.VerifyHeader` and `eip1559.VerifyEIP1559Header`: before any state is touched.
    match (ctx.verify, ctx.derive) {
        (VerifyContext::V2(c), Some(derive)) => {
            HeaderVerifier::new(spec).verify_derived(header, ctx.parent, u64::MAX, c, derive)?
        }
        _ => HeaderVerifier::new(spec).verify(header, ctx.parent, u64::MAX, ctx.verify)?,
    }
    basefee::verify(spec, header.number, header.base_fee)?;
    let author = block_author(spec, header)?;
    run_block(spec, parent_view, header, author, body, ctx.parent, ctx.env)
}

/// The producer's execution of a block it is building (XB-23): everything `execute_block` does after the
/// header checks, for a header whose roots and seal do not exist yet. `author` is the creator that the
/// seal will name. The sealed block is imported through `execute_block` in full, so no check is skipped
/// for a block that leaves the node: this only computes the roots to put in the header.
pub fn execute_unsealed<V: StateRead, E: Env>(
    spec: &ChainSpec,
    parent_view: &V,
    header: &Header,
    author: Address,
    body: &Body,
    parent: &Header,
    env: &E,
) -> Result<BlockOutcome, ExecError> {
    basefee::verify(spec, header.number, header.base_fee)?;
    run_block(spec, parent_view, header, author, body, parent, env)
}

fn run_block<V: StateRead, E: Env>(
    spec: &ChainSpec,
    parent_view: &V,
    header: &Header,
    author: Address,
    body: &Body,
    parent: &Header,
    env: &E,
) -> Result<BlockOutcome, ExecError> {
    let number = header.number;
    let r = rules(spec, number)?;
    let eip158 = spec.eip158.is_active(number);
    let txs = body
        .transactions
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            tx::decode(raw, spec.chain_id).map_err(|error| ExecError::Tx { index, error })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut overlay = Overlay::new(parent_view, env);
    // `ApplyTIPSigningHardFork`, `core/state_processor.go:152`, `:321-327`: before the first transaction.
    if let Some(wiped) = tip_signing_wipe(spec, number) {
        overlay.delete(wiped);
    }
    if spec.prague.is_active(number) {
        prague_pre_execution(&mut overlay, number, header.parent_hash)?;
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
    for (a, w) in env.wide_balances() {
        evm.ctx.journaled_state.side.insert(a, w);
    }

    // TRC21 token fees: the capacity of every registered token is read once from the state before the
    // block and drawn down as the block executes (`core/state_processor.go:184-191`).
    let issuer = trc21_issuer::issuer(spec.chain_id);
    let mut tokens_fee = trc21_issuer::capacities(parent_view, issuer);
    let mut fee_updated: BTreeMap<Address, U256> = BTreeMap::new();
    let mut total_fee_used = U256::ZERO;
    let token_price = fees::trc21_gas_price(spec, number);

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
                // `ApplyTRC21TokenFee`, `core/state_processor.go:977-993`
                let token =
                    t.to.and_then(|to| tokens_fee.get(&to).map(|capacity| (to, *capacity)));
                evm.ctx.chain.token_fee = token.map(|(_, capacity)| capacity);
                let mut env = tx_env(spec, t);
                if token.is_some() {
                    env.gas_price = token_price;
                    env.gas_priority_fee = env.gas_priority_fee.map(|_| token_price);
                }
                evm.ctx.set_tx(env);
                let result = XdcHandler::<Overlay<'_, V, E>>::default()
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
                // `SettleTRC21TokenFee`, `core/state_processor.go:999-1010`
                if let Some((token, capacity)) = token {
                    let db = evm.ctx.journaled_state.db_and_state_mut().0;
                    if !success {
                        pay_fee_on_failure(db, t.from, token);
                    }
                    let fee = U256::from(gas) * U256::from(token_price);
                    let left = capacity.checked_sub(fee).ok_or(ExecError::Unsupported(
                        "a TRC21 fee above the token capacity",
                    ))?;
                    tokens_fee.insert(token, left);
                    fee_updated.insert(token, left);
                    total_fee_used += fee;
                }
                used += gas;
                receipts.push(Receipt::new(t.ty, success, used, gas, logs));
            }
        }
    }

    let db = evm.ctx.journaled_state.db_and_state_mut().0;
    // `UpdateTRC21Fee`, `core/state/trc21_reader.go:135-148`: after the loop, before `Finalize`.
    if !fee_updated.is_empty() {
        for (token, capacity) in &fee_updated {
            db.set_slot(issuer, trc21_issuer::capacity_slot(*token), *capacity);
        }
        db.debit(issuer, total_fee_used, eip158)?;
    }
    pay_checkpoint_reward(spec, parent, env, parent_view, header, db, eip158)?;

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
    let state_root = StateRead::root_with(parent_view, &diff)?;
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

/// geth `PayFeeWithTRC21TxFail` (`core/state/trc21_reader.go:71-91`): a failed token-fee
/// transaction costs its sender the token's `minFee` (at most the sender's token balance), paid to
/// the token's issuer.
fn pay_fee_on_failure<S: xone_state::StateRead, E: Env>(
    db: &mut Overlay<'_, S, E>,
    from: Address,
    token: Address,
) {
    let balance_key = trc21_issuer::balance_slot(from);
    let balance = db.slot(token, balance_key);
    if balance.is_zero() {
        return;
    }
    let issuer = trc21_issuer::token_issuer(db.slot(token, trc21_issuer::token_issuer_slot()));
    let fee = db.slot(token, trc21_issuer::min_fee_slot());
    let used = fee.min(balance);
    db.set_slot(token, balance_key, balance - used);
    let issuer_key = trc21_issuer::balance_slot(issuer);
    let issuer_balance = db.slot(token, issuer_key);
    db.set_slot(token, issuer_key, issuer_balance.wrapping_add(used));
}

/// geth `ApplyMulticall3HardFork` and `ProcessParentBlockHash` (`core/state_processor.go:101-120`,
/// `:591-664`) on a Prague block whose state already holds both contracts: the history contract
/// stores the parent hash at `(number - 1) % 8191` (EIP-2935, `historyStorageKey`, `:672`). The
/// Prague block itself deploys the two contracts, which is not implemented.
fn prague_pre_execution<S: xone_state::StateRead, E: Env>(
    db: &mut Overlay<'_, S, E>,
    number: u64,
    parent: B256,
) -> Result<(), ExecError> {
    const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");
    const HISTORY: Address = address!("0000F90827F1C53a10cb7A02335B175320002935");
    const WINDOW: u64 = 8191;
    let has_code = |a: Address| {
        db.account(a)
            .is_some_and(|x| x.code_hash != alloy_primitives::KECCAK256_EMPTY)
    };
    if !has_code(MULTICALL3) || !has_code(HISTORY) {
        return Err(ExecError::Unsupported(
            "the Prague block deploy of Multicall3 and the EIP-2935 history contract",
        ));
    }
    let slot = B256::from(U256::from((number - 1) % WINDOW).to_be_bytes::<32>());
    db.set_slot(HISTORY, slot, U256::from_be_bytes(parent.0));
    Ok(())
}

/// The reward of a checkpoint or epoch switch, paid from the state read before the transactions
/// (`Finalize`, `consensus/XDPoS/xdpos.go:1498`, `:1593-1612`; V2: `engine_v2/engine.go:769-797`).
///
/// A V1 block (the V2 switch block included) pays when `number % RewardCheckpoint == 0`, a V2 block
/// when it is an epoch switch. Then `HookReward` (`eth/hooks/engine_v2_hooks.go:60-305`): nothing at
/// the first V2 block (`:76`) or up to the first checkpoint (`:81`); a V1 block counts its fixed
/// window (`GetSigningTxCount`, `:621`, V1 branch), a V2 block the window of the epoch switches
/// (`:680-720`, `:949-995`); the tiered rule from `TIPFixedReward` (`:93`) also records the minted
/// and burnt totals in the MintedRecord contract (`:173-212`).
fn pay_checkpoint_reward<V: StateRead, E: Env>(
    spec: &ChainSpec,
    parent: &Header,
    env: &E,
    parent_view: &V,
    header: &Header,
    db: &mut Overlay<'_, V, E>,
    eip158: bool,
) -> Result<(), ExecError> {
    let number = header.number;
    let period = reward_v1::checkpoint_period(spec);
    let v2 = spec.xdpos.v2.as_ref();
    let is_v2_block = v2.is_some_and(|v| number > v.switch_block);
    let due = match v2 {
        Some(v) if is_v2_block => epoch_switch::of_header(v, spec.xdpos.epoch, header)?.0,
        _ => number.is_multiple_of(period),
    };
    if !due || number <= period || v2.is_some_and(|v| number == v.switch_block.saturating_add(1)) {
        return Ok(());
    }
    let root = parent.state_root;
    let missing = |n| chain::Error::Missing {
        what: "epoch-switch header",
        number: n,
        hash: B256::ZERO,
    };
    let paid: reward::RewardMap;
    let mut burned = U256::ZERO;
    let mut epoch_number = 0;
    if !is_v2_block {
        let at = reward_v1::locate(spec, number)?;
        let blocks = SignTxCounter::new(env).collect((header.parent_hash, number - 1), at.start)?;
        let masternodes = env
            .masternodes_at(at.checkpoint)
            .ok_or(missing(at.checkpoint))?;
        let window = RewardWindow::new(at.start, at.end, blocks, masternodes, vec![], vec![])?;
        let holders = Holders::read(root, parent_view, window.masternodes().iter().copied());
        paid = reward_v1::rewards(spec, number, &window, &holders, root)?;
    } else {
        let at = reward_v2::locate(spec, env, number, header.parent_hash)?;
        let blocks = SignTxCounter::new(env).collect((header.parent_hash, number - 1), at.start)?;
        let masternodes = env
            .masternodes_at(at.switch.1)
            .ok_or(missing(at.switch.1))?;
        match reward::rule(spec, number) {
            Rule::V1 => return Err(ExecError::Unsupported("a V1 reward rule in a V2 block")),
            Rule::V2 => {
                let window =
                    RewardWindow::new(at.start, at.end, blocks, masternodes, vec![], vec![])?;
                let holders =
                    Holders::read(root, parent_view, window.masternodes().iter().copied());
                paid = reward_v2::rewards(spec, number, &window, &holders, root)?;
            }
            Rule::Tiered { .. } => {
                let round = header
                    .extra_fields_v2()
                    .map_err(|_| ExecError::NoV2Fields(number))?
                    .round;
                let cfg = tiered::config(spec, round)?;
                let pool = env
                    .standby_pool(at.switch.1)
                    .ok_or(ExecError::NoStandby(at.switch.1))?;
                let (protectors, observers) = tiered::tiers(cfg, &pool);
                let nodes: Vec<Address> = masternodes
                    .iter()
                    .chain(&protectors)
                    .chain(&observers)
                    .copied()
                    .collect();
                let window = RewardWindow::new(
                    at.start,
                    at.end,
                    blocks,
                    masternodes,
                    protectors,
                    observers,
                )?;
                let holders = Holders::read(root, parent_view, nodes);
                paid = tiered::rewards(spec, number, cfg, &window, &holders, root)?;
                for n in at.end + 1..number {
                    burned += env.burn(n).ok_or(ExecError::NoBurn(n))?;
                }
                let v = v2.ok_or(chain::Error::NoV2)?;
                epoch_number = v.switch_epoch + round / spec.xdpos.epoch;
            }
        }
    }
    let mut sum = U256::ZERO;
    for (holder, amount) in paid {
        db.credit(holder, amount, eip158)?;
        sum += amount;
    }
    if matches!(reward::rule(spec, number), Rule::Tiered { .. }) && is_v2_block {
        record_minted(db, epoch_number, number, sum, burned)?;
    }
    Ok(())
}

/// The MintedRecord bookkeeping of the tiered reward (`engine_v2_hooks.go:173-212`): the first
/// record stores the onset epoch and block, later ones continue the totals of the nearest earlier
/// epoch that has any; the nonce is incremented so the account is never empty.
fn record_minted<S: xone_state::StateRead, E: Env>(
    db: &mut Overlay<'_, S, E>,
    epoch: u64,
    number: u64,
    reward_sum: U256,
    burned_in_epoch: U256,
) -> Result<(), ExecError> {
    let a = minted::ADDRESS;
    let nonce = db.account(a).map_or(0, |x| x.nonce);
    let (mut total_minted, mut total_burned) = (U256::ZERO, U256::ZERO);
    if nonce == 0 {
        db.set_slot(a, minted::onset_epoch(), U256::from(epoch));
        db.set_slot(a, minted::onset_block(), U256::from(number));
    } else {
        let mut e = epoch;
        while e > 0 {
            e -= 1;
            total_minted = db.slot(a, minted::post_minted(e));
            total_burned = db.slot(a, minted::post_burned(e));
            if !total_minted.is_zero() || !total_burned.is_zero() {
                break;
            }
        }
    }
    // the total saturates at 2^256 - 1 (`engine_v2_hooks.go:196`, `:207`)
    total_minted = total_minted.saturating_add(reward_sum);
    db.set_slot(a, minted::post_minted(epoch), total_minted);
    db.set_slot(a, minted::post_reward_block(epoch), U256::from(number));
    total_burned = total_burned.saturating_add(burned_in_epoch);
    db.set_slot(a, minted::post_burned(epoch), total_burned);
    db.set_nonce(a, nonce + 1)?;
    Ok(())
}
