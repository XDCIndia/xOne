//! Admission checks against the head. geth `core/txpool/validation.go` (`ValidateTransaction`,
//! `ValidateTransactionWithState`) as `legacypool.ValidateTxBasics` and `validateTx` call them.
//!
//! The checks run in geth's order, so a transaction that breaks two rules is refused for the same one.

use super::{tier_price, Error, Pool, PoolTx, TX_MAX_SIZE};
use alloy_primitives::{Address, KECCAK256_EMPTY, U256};
use xone_state::StateRead;

/// `params.MaxInitCodeSize`, `params/protocol_params.go:178`.
pub const MAX_INIT_CODE_SIZE: u64 = 2 * 24_576;
/// `params.MaxTxGas` (EIP-7825), `params/protocol_params.go:39`.
pub const MAX_TX_GAS: u64 = 1 << 24;
/// `common.DefaultMinGasPrice`, `common/constants.go:15`: the floor before the `Gas50x` tier.
pub const DEFAULT_MIN_GAS_PRICE: u128 = 250_000_000;

/// The gas price a regular transaction must offer for the next block (`validation.go:406-422`): the tier
/// price from `Gas50x` (12.5 gwei, 625 gwei from `Gas2500x`), `DEFAULT_MIN_GAS_PRICE` before. A special
/// transaction is exempt.
pub fn min_gas_price(spec: &xone_chainspec::ChainSpec, head: u64) -> u128 {
    let pending = head.saturating_add(1);
    if spec.effective.gas50x.is_active(pending) {
        tier_price(spec, pending)
    } else {
        DEFAULT_MIN_GAS_PRICE
    }
}

/// `ValidateTxBasics` (`legacypool.go:585-603`): everything that needs the head header but not the state.
pub fn basics(pool: &Pool, tx: &PoolTx) -> Result<(), Error> {
    let t = &tx.tx;
    let spec = &pool.spec;
    let head = &pool.head;
    if tx.size > TX_MAX_SIZE {
        return Err(Error::Oversized {
            size: tx.size,
            limit: TX_MAX_SIZE,
        });
    }
    // Type gates (`validation.go:88-99`). Types 3 and 4 never get here: the decoder does not know them.
    if t.ty != 0 {
        if !spec.berlin.is_active(head.number) {
            return Err(Error::TypeNotSupported(format!(
                "type {} rejected, pool not yet in Berlin",
                t.ty
            )));
        }
        if !spec.is_eip1559(head.number) {
            return Err(Error::TypeNotSupported(format!(
                "type {} rejected, XDC pool not yet at EIP-1559",
                t.ty
            )));
        }
        if t.ty == 2 && !spec.london.is_active(head.number) {
            return Err(Error::TypeNotSupported(format!(
                "type {} rejected, pool not yet in London",
                t.ty
            )));
        }
    }
    if t.to.is_none() && spec.is_eip3860(head.number) {
        let size = u64::try_from(t.data.len()).unwrap_or(u64::MAX);
        if size > MAX_INIT_CODE_SIZE {
            return Err(Error::MaxInitCodeSize {
                size,
                limit: MAX_INIT_CODE_SIZE,
            });
        }
    }
    if spec.osaka.is_active(head.number) && t.gas > MAX_TX_GAS {
        return Err(Error::GasLimitTooHigh {
            cap: MAX_TX_GAS,
            gas: t.gas,
        });
    }
    if head.gas_limit < t.gas {
        return Err(Error::GasLimit);
    }
    if tx.fee_cap() < tx.tip_cap() {
        return Err(Error::TipAboveFeeCap);
    }
    if t.nonce == u64::MAX {
        return Err(Error::NonceMax);
    }
    // The sender was recovered when the transaction was decoded.
    if tx.is_special() && !pool.authority.is_signer(&t.from) {
        return Err(Error::SpecialTxNotFromSigner(t.from));
    }
    let create = t.to.is_none();
    let (accounts, keys) = access_list_counts(tx);
    let (intr, floor) = head.gas.of(
        &t.data,
        create,
        accounts,
        keys,
        xone_evm::gas::eip2780(&head.rules, t.value, t.to == Some(t.from)),
    );
    if t.gas < intr {
        return Err(Error::IntrinsicGas {
            gas: t.gas,
            need: intr,
        });
    }
    if spec.prague.is_active(head.number) && t.gas < floor {
        return Err(Error::FloorDataGas {
            gas: t.gas,
            need: floor,
        });
    }
    // A special transaction is zero-priced by design (`validation.go:198-213`).
    if !tx.is_special() && tx.tip_cap() < pool.min_tip {
        return Err(Error::GasPriceTooLow {
            tip: tx.tip_cap(),
            min: pool.min_tip,
        });
    }
    Ok(())
}

fn access_list_counts(tx: &PoolTx) -> (u64, u64) {
    let al = &tx.tx.access_list;
    let accounts = u64::try_from(al.len()).unwrap_or(u64::MAX);
    let keys = al
        .iter()
        .map(|i| u64::try_from(i.keys.len()).unwrap_or(u64::MAX))
        .sum();
    (accounts, keys)
}

/// The blacklist part of `validateTx` (`legacypool.go:640-655`): once the head is at or past the
/// denylist block, a listed sender, then a listed recipient, is refused.
pub fn denylist(spec: &xone_chainspec::ChainSpec, head: u64, tx: &PoolTx) -> Result<(), Error> {
    if !spec.is_denylist(head) {
        return Ok(());
    }
    let from = tx.from();
    if xone_chainspec::is_denylisted(&from) {
        return Err(Error::Denylisted {
            side: "sender",
            address: from,
        });
    }
    match tx.tx.to {
        Some(to) if xone_chainspec::is_denylisted(&to) => Err(Error::Denylisted {
            side: "recipient",
            address: to,
        }),
        _ => Ok(()),
    }
}

/// `validateTx` (`legacypool.go:640-698`) without the TRC21 part: the blacklist ([`denylist`]) first, then
/// nonce and balance against the head state with what the pool already holds for the sender, the tier
/// floor, and the limit on an account that has code. The balance is the low 256 bits, as
/// `StateDB.GetBalance` returns it.
pub fn with_state<S: StateRead>(pool: &mut Pool, state: &S, tx: &PoolTx) -> Result<(), Error> {
    denylist(&pool.spec, pool.head.number, tx)?;
    let from = tx.from();
    let acct = state.account(from);
    let next = acct.as_ref().map_or(0, |a| a.nonce);
    if next > tx.tx.nonce {
        return Err(Error::NonceTooLow {
            next,
            tx: tx.tx.nonce,
        });
    }
    let mut balance = acct.as_ref().map_or(U256::ZERO, |a| a.balance.low());
    if pool.trc21.at != Some(pool.head.number) {
        pool.trc21 = super::Trc21Fees::read(&pool.spec, state, pool.head.number);
    }
    // geth-XDC `ValidateTransactionWithState`: to a registered TRC21 token, the token's fee
    // capacity adds to the balance and the cost is at the TRC21 gas price of the pending block.
    let cost = match (pool.trc21.cost(tx), tx.tx.to) {
        (Some((cap, cost)), Some(to)) => {
            if !xone_state::system_contracts::trc21_issuer::validate_trc21_tx(
                state,
                from,
                to,
                tx.tx.data.as_ref(),
            ) {
                return Err(Error::InsufficientFunds);
            }
            balance = balance.saturating_add(cap);
            cost.ok_or(Error::InsufficientFunds)?
        }
        _ => tx.cost().ok_or(Error::InsufficientFunds)?,
    };
    if balance < cost {
        return Err(Error::InsufficientFunds);
    }
    let list = pool.pending.get(&from);
    let spent = list.map_or(U256::ZERO, super::list::List::total_cost);
    let need = match list.and_then(|l| l.get(tx.tx.nonce)) {
        // A replacement: the old cost is already in `spent`.
        Some(prev) => spent
            .saturating_sub(prev.cost().unwrap_or(U256::MAX))
            .checked_add(cost),
        None => spent.checked_add(cost),
    };
    if need.is_none_or(|n| balance < n) {
        return Err(Error::InsufficientFunds);
    }
    if !tx.is_special() && tx.fee_cap() < min_gas_price(&pool.spec, pool.head.number) {
        return Err(Error::UnderMinGasPrice);
    }
    check_delegation_limit(pool, state, acct.as_ref().map(|a| a.code_hash), from, tx)
}

/// `checkDelegationLimit` (`legacypool.go:700-739`): an account with code (an EIP-7702 delegation) may have
/// one executable transaction in flight, and none out of order.
fn check_delegation_limit<S: StateRead>(
    pool: &mut Pool,
    state: &S,
    code_hash: Option<alloy_primitives::B256>,
    from: Address,
    tx: &PoolTx,
) -> Result<(), Error> {
    // No state object, or the empty code hash: nothing to limit.
    if code_hash.is_none_or(|h| h == KECCAK256_EMPTY || h.is_zero()) {
        return Ok(());
    }
    match pool.pending.get(&from) {
        None => {
            if pool.nonces.get(state, from) != tx.tx.nonce {
                return Err(Error::OutOfOrderTxFromDelegated);
            }
            Ok(())
        }
        Some(p) if p.contains(tx.tx.nonce) => Ok(()),
        Some(_) => Err(Error::InflightTxLimitReached),
    }
}
