//! Block execution for xOne: one block on the state of its parent, in the order of geth-XDC
//! `StateProcessor.Process` (`core/state_processor.go:115-290`), and nothing copied from the header.
//! The commitments of the result (transactions root, receipts root, bloom, state root, gas used)
//! are computed here from the body and the executed state; the caller compares them with the
//! header. The order is written down in `spec/execution/block-processing-order.md`.
#![forbid(unsafe_code)]

mod bal;
mod overlay;
pub mod tx;

pub use overlay::OverlayError;
pub use tx::{Tx, TxError};

use alloy_primitives::{address, Address, Bloom, Bytes, B256, U256};
use alloy_trie::root::ordered_trie_root_encoded;
use bal::Bal;
use overlay::Overlay;
use revm::{
    context::{result::EVMError, ContextSetters, TxEnv},
    context_interface::{result::ExecutionResult, JournalTr},
    handler::Handler,
    primitives::TxKind,
};
use std::collections::BTreeMap;
use xone_chainspec::{ChainSpec, ForkBlock};
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
    /// From Amsterdam: the hash of the block access list this execution built (geth
    /// `ValidateState`, `core/block_validator.go:239-253`); the caller compares it with the
    /// header's `blockAccessListHash`.
    pub bal_hash: Option<B256>,
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
    let number = header.number;
    // geth `engine.VerifyHeader` and `eip1559.VerifyEIP1559Header`: before any state is touched.
    match (ctx.verify, ctx.derive) {
        (VerifyContext::V2(c), Some(derive)) => {
            HeaderVerifier::new(spec).verify_derived(header, ctx.parent, u64::MAX, c, derive)?
        }
        _ => HeaderVerifier::new(spec).verify(header, ctx.parent, u64::MAX, ctx.verify)?,
    }
    basefee::verify(spec, number, header.base_fee)?;
    let r = rules(spec, number)?;
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
    let amsterdam = spec.amsterdam.is_active(number);
    let mut bal = amsterdam.then(Bal::default);
    if spec.prague.is_active(number) {
        let slot = ring_slot(number - 1);
        let before = overlay.slot(HISTORY, slot);
        prague_pre_execution(spec, &mut overlay, ctx.env, number, header.parent_hash)?;
        // EIP-7928: the system call is block access index 0 (`core/state_processor.go:589-664`).
        if let Some(b) = bal.as_mut() {
            if before == overlay.slot(HISTORY, slot) {
                b.storage_read(HISTORY, slot);
            } else {
                b.storage_write(0, HISTORY, slot, header.parent_hash);
            }
        }
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

    // TRC21 token fees: the capacity of every registered token is read once from the state before the
    // block and drawn down as the block executes (`core/state_processor.go:184-191`).
    let issuer = trc21_issuer::issuer(spec.chain_id);
    let mut tokens_fee = trc21_issuer::capacities(parent_view, issuer);
    let mut fee_updated: BTreeMap<Address, U256> = BTreeMap::new();
    let mut total_fee_used = U256::ZERO;
    let token_price = fees::trc21_gas_price(spec, number);

    let mut receipts: Vec<Receipt> = Vec::with_capacity(txs.len());
    let mut used = 0u64;
    // EIP-8037 two-dimensional block gas (`core/gaspool.go:92-148`): the block uses the larger of the
    // execution and state sums; the receipts carry the sum of what each transaction paid.
    let (mut exec_sum, mut state_sum) = (0u64, 0u64);
    // Accounts a special transaction changed since the last `Finalise` and their nonce before it:
    // the sign path finalises without the access list (`core/state_processor.go:886`, rules without
    // Amsterdam), so its change is recorded by the next transaction's finalise, or the post phase.
    let mut pending: BTreeMap<Address, u64> = BTreeMap::new();
    for (index, t) in txs.iter().enumerate() {
        let bal_index = u32::try_from(index + 1).unwrap_or(u32::MAX);
        // `RouteTransaction`, `core/state_processor.go:856-872`, used at `:235-258`.
        match route(&r, t.to) {
            Route::SignTx => {
                let db = evm.ctx.journaled_state.db_and_state_mut().0;
                let state_nonce = db.account(t.from).map_or(0, |a| a.nonce);
                let cumulative = if amsterdam { exec_sum.max(state_sum) } else { used };
                pending.clear();
                pending.insert(t.from, state_nonce);
                let (nonce, receipt) = apply_sign_tx(spec, number, state_nonce, t.nonce, cumulative)
                    .map_err(|error| ExecError::Special { index, error })?;
                db.set_nonce(t.from, nonce)?;
                receipts.push(receipt);
            }
            Route::XdcxEmpty if amsterdam => {
                return Err(ExecError::Unsupported("an XDCx empty transaction in Amsterdam"))
            }
            Route::XdcxEmpty => {
                let to =
                    t.to.ok_or(ExecError::Unsupported("empty transaction without a target"))?;
                let receipt = apply_empty_tx(spec, number, to, used)
                    .map_err(|error| ExecError::Special { index, error })?;
                receipts.push(receipt);
            }
            Route::RandomizeTx | Route::Evm => {
                // `gp.SubGas(msg.GasLimit)`, `core/state_transition.go`: the pool must hold the whole limit;
                // from Amsterdam `CheckGasAmsterdam` (`core/gaspool.go:64-74`): the execution part (the
                // limit capped by `MaxTxGas`) and the state part must each fit their dimension.
                const MAX_TX_GAS: u64 = 1 << 24;
                let fits = if amsterdam {
                    t.gas.min(MAX_TX_GAS) <= header.gas_limit.saturating_sub(exec_sum)
                        && t.gas <= header.gas_limit.saturating_sub(state_sum)
                } else {
                    t.gas <= header.gas_limit.saturating_sub(used)
                };
                if !fits {
                    return Err(ExecError::GasLimitReached { index });
                }
                // EIP-7928 for a plain transfer: the accounts the transaction loads and changes.
                let bal_touched: Vec<(Address, Option<xone_state::Account>)> = if amsterdam {
                    let db = evm.ctx.journaled_state.db_and_state_mut().0;
                    let to = t
                        .to
                        .filter(|to| {
                            // not a precompile or system address, no code, no data, no token fee
                            to.0[..18] != [0u8; 18]
                                && t.data.is_empty()
                                && t.access_list.is_empty()
                                && db
                                    .account(*to)
                                    .is_none_or(|a| a.code_hash == alloy_primitives::KECCAK256_EMPTY)
                                && !tokens_fee.contains_key(to)
                        })
                        .ok_or(ExecError::Unsupported(
                            "the block access list of a transaction that is not a plain transfer",
                        ))?;
                    let mut v = vec![t.from, to];
                    v.extend(owner);
                    v.sort();
                    v.dedup();
                    v.into_iter().map(|a| (a, db.account(a))).collect()
                } else {
                    vec![]
                };
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
                let (gas, split, success, logs) = match result {
                    ExecutionResult::Success { gas, logs, .. } => {
                        (gas.tx_gas_used(), gas, true, logs)
                    }
                    ExecutionResult::Revert { gas, .. } | ExecutionResult::Halt { gas, .. } => {
                        (gas.tx_gas_used(), gas, false, vec![])
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
                exec_sum += split.block_regular_gas_used();
                state_sum += split.block_state_gas_used();
                if let Some(b) = bal.as_mut() {
                    let db = evm.ctx.journaled_state.db_and_state_mut().0;
                    let post = |a: Address| db.account(a);
                    let (nonce_of, balance_of) = (
                        |x: &Option<xone_state::Account>| x.as_ref().map_or(0, |x| x.nonce),
                        |x: &Option<xone_state::Account>| {
                            x.as_ref().map_or(U256::ZERO, |x| x.balance.low())
                        },
                    );
                    for (a, before) in &bal_touched {
                        let after = post(*a);
                        b.read(*a);
                        if balance_of(before) != balance_of(&after) {
                            b.balance(bal_index, *a, balance_of(&after));
                        }
                        if nonce_of(before) != nonce_of(&after) {
                            b.nonce(bal_index, *a, nonce_of(&after));
                        }
                    }
                    for (a, was) in std::mem::take(&mut pending) {
                        let now = nonce_of(&post(a));
                        if now != was && !bal_touched.iter().any(|(t, _)| *t == a) {
                            b.nonce(bal_index, a, now);
                        }
                    }
                }
                receipts.push(Receipt::new(t.ty, success, used, gas, logs));
            }
        }
    }

    let db = evm.ctx.journaled_state.db_and_state_mut().0;
    // EIP-7928 post-execution phase, index `n + 1`: `ProcessWithdrawals` finalises what a sign
    // transaction left (`core/state_processor.go:403-420`); the rewards below come after it and are
    // not part of the list.
    if let Some(b) = bal.as_mut() {
        let post_index = u32::try_from(txs.len() + 1).unwrap_or(u32::MAX);
        for (a, was) in std::mem::take(&mut pending) {
            let now = db.account(a).map_or(0, |x| x.nonce);
            if now != was {
                b.nonce(post_index, a, now);
            }
        }
    }
    // `UpdateTRC21Fee`, `core/state/trc21_reader.go:135-148`: after the loop, before `Finalize`.
    if !fee_updated.is_empty() {
        for (token, capacity) in &fee_updated {
            db.set_slot(issuer, trc21_issuer::capacity_slot(*token), *capacity);
        }
        db.debit(issuer, total_fee_used, eip158)?;
    }
    pay_checkpoint_reward(spec, ctx, parent_view, header, db, eip158)?;

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
        gas_used: if amsterdam {
            exec_sum.max(state_sum)
        } else {
            used
        },
        bloom,
        state_root,
        receipt_root,
        tx_root,
        bal_hash: bal.map(|b| b.hash()),
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

const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");
const HISTORY: Address = address!("0000F90827F1C53a10cb7A02335B175320002935");

/// geth `PreExecution` on a Prague block (`core/state_processor.go:331-353`), in its order:
///
/// 1. `ApplyMulticall3HardFork` (`:62-93`, not on chain 5551): the canonical Multicall3 runtime code
///    (`params/multicall3.go`) is installed where the address has none, with nonce 1; balance and
///    storage stay. Other code at the address is a panic in geth, an error here.
/// 2. `ProcessParentBlockHash` (`:589-664`): EIP-2935. Where the history contract has no code and
///    the block is at or after `PragueBlock` (XDC has no history contract in genesis, `:603-608`) it
///    is deployed: account created, nonce 1 if 0, code `params.HistoryStorageCode`
///    (`params/protocol_params.go:271`), and the ring buffer is back-filled with the hashes of
///    blocks `max(number - 8191, pragueBlock - 1) ..= number - 1` (`:618-636`, a block without a
///    known hash is skipped). Then the system call stores the parent hash at `(number - 1) % 8191`
///    (`:670-677`, the contract's own code does the same SSTORE).
fn prague_pre_execution<S: xone_state::StateRead, E: Env>(
    spec: &ChainSpec,
    db: &mut Overlay<'_, S, E>,
    env: &E,
    number: u64,
    parent: B256,
) -> Result<(), ExecError> {
    // `params.HistoryStorageCode`, `params/protocol_params.go:271`
    const HISTORY_CODE: [u8; 83] = alloy_primitives::hex!(
        "3373fffffffffffffffffffffffffffffffffffffffe14604657602036036042575f35600143038111604257611fff81430311604257611fff9006545f5260205ff35b5f5ffd5b5f35611fff60014303065500"
    );
    const WINDOW: u64 = 8191;
    let code_of = |db: &Overlay<'_, S, E>, a: Address| {
        db.account(a)
            .filter(|x| x.code_hash != alloy_primitives::KECCAK256_EMPTY)
    };
    if spec.installs_multicall3(number) {
        let want = Bytes::from(
            alloy_primitives::hex::decode(include_str!("multicall3.hex").trim())
                .map_err(|_| ExecError::Unsupported("the Multicall3 runtime code"))?,
        );
        match code_of(db, MULTICALL3) {
            None => {
                db.set_code(MULTICALL3, want)?;
                db.set_nonce(MULTICALL3, 1)?;
            }
            Some(x) if x.code_hash == alloy_primitives::keccak256(&want) => {}
            Some(_) => return Err(ExecError::Unsupported("other code at the Multicall3 address")),
        }
    }
    match code_of(db, HISTORY) {
        Some(x) if x.code_hash != alloy_primitives::keccak256(HISTORY_CODE) => {
            return Err(ExecError::Unsupported("other code at the EIP-2935 address"))
        }
        Some(_) => {}
        None => {
            let ForkBlock::At(fork) = spec.prague else {
                return Err(ExecError::Unsupported("Prague without a block number"));
            };
            db.set_code(HISTORY, Bytes::from_static(&HISTORY_CODE))?;
            if db.account(HISTORY).is_some_and(|x| x.nonce == 0) {
                db.set_nonce(HISTORY, 1)?;
            }
            let end = number - 1;
            let mut start = if end + 1 > WINDOW { end + 1 - WINDOW } else { end };
            if fork > 0 {
                start = start.max(fork - 1);
            }
            for n in start..=end {
                if let Some(h) = env.hash_of(n).filter(|h| !h.is_zero()) {
                    db.set_slot(HISTORY, ring_slot(n), U256::from_be_bytes(h.0));
                }
            }
        }
    }
    db.set_slot(HISTORY, ring_slot(number - 1), U256::from_be_bytes(parent.0));
    Ok(())
}

/// `historyStorageKey` (`core/state_processor.go:672`): block number modulo 8191, as a 32-byte key.
fn ring_slot(number: u64) -> B256 {
    B256::from(U256::from(number % 8191).to_be_bytes::<32>())
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
    ctx: &ExecCtx<'_, E>,
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
    let root = ctx.parent.state_root;
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
        let blocks =
            SignTxCounter::new(ctx.env).collect((header.parent_hash, number - 1), at.start)?;
        let masternodes = ctx
            .env
            .masternodes_at(at.checkpoint)
            .ok_or(missing(at.checkpoint))?;
        let window = RewardWindow::new(at.start, at.end, blocks, masternodes, vec![], vec![])?;
        let holders = Holders::read(root, parent_view, window.masternodes().iter().copied());
        paid = reward_v1::rewards(spec, number, &window, &holders, root)?;
    } else {
        let at = reward_v2::locate(spec, ctx.env, number, header.parent_hash)?;
        let blocks =
            SignTxCounter::new(ctx.env).collect((header.parent_hash, number - 1), at.start)?;
        let masternodes = ctx
            .env
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
                let pool = ctx
                    .env
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
                    burned += ctx.env.burn(n).ok_or(ExecError::NoBurn(n))?;
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
