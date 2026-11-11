#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)] // integration test: a panic is the failure report
//! A special transaction (to 0x89 or 0x90) is exempt from the base-fee floor and pays no fee
//! (geth `core/state_transition.go:684` and `:881`). Apothem has no such transaction after
//! `Eip1559Block` (V1 ends before it), so the rule is checked on the handler: the same zero-price
//! transaction is accepted to 0x90 and to 0x89 (before `TIPSigning`) and rejected to anything else.

use alloy_primitives::{Address, U256};
use revm::{
    context::{
        result::{EVMError, InvalidTransaction},
        TxEnv,
    },
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::Handler,
    primitives::TxKind,
    state::AccountInfo,
};
use xone_chainspec::{ForkBlock, Network};
use xone_evm::{
    basefee,
    handler::{XdcHandler, XdcTxError},
    route::{BLOCK_SIGNERS, RANDOMIZE},
    spec::XdcBlock,
};

/// Runs a zero-price call to `to` at the first block with a base fee; returns the error or the
/// accounts the transaction touched.
fn run(to: Address, pre_tip_signing: bool) -> Result<Vec<Address>, String> {
    let spec = Network::Apothem.spec();
    let ForkBlock::At(n) = spec.eip1559 else {
        panic!()
    };
    let sender = Address::repeat_byte(0xf1);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: U256::from(10).pow(U256::from(20)),
            ..Default::default()
        },
    );
    let block = XdcBlock {
        number: n,
        gas_limit: 30_000_000,
        difficulty: U256::from(1),
        base_fee: basefee::base_fee_for_block(&spec, n),
        beneficiary: Address::repeat_byte(0xc0),
        ..Default::default()
    };
    let mut rules = xone_evm::spec::rules(&spec, n).unwrap();
    rules.sign_tx_route = !pre_tip_signing;
    let mut evm = xone_evm::handler::build_evm_with_rules(
        db,
        &spec,
        rules,
        &block,
        Some(Address::repeat_byte(0xd0)),
    );
    evm.ctx.tx = TxEnv {
        caller: sender,
        kind: TxKind::Call(to),
        gas_limit: 100_000,
        gas_price: 0,
        chain_id: None,
        ..Default::default()
    };
    match XdcHandler::default().run(&mut evm) {
        Ok(_) => Ok(evm.ctx.journaled_state.finalize().keys().copied().collect()),
        Err(EVMError::Transaction(XdcTxError::Invalid(
            InvalidTransaction::GasPriceLessThanBasefee,
        ))) => Err("GasPriceLessThanBasefee".into()),
        Err(e) => Err(format!("{e}")),
    }
}

#[test]
fn zero_price_special_txs_are_accepted_and_credit_no_fee() {
    for to in [RANDOMIZE, BLOCK_SIGNERS] {
        let touched = run(to, true).unwrap_or_else(|e| panic!("{to}: {e}"));
        assert!(
            !touched.contains(&Address::repeat_byte(0xc0)),
            "the coinbase is not touched"
        );
        assert!(
            !touched.contains(&Address::repeat_byte(0xd0)),
            "the owner is not touched"
        );
    }
}

/// Named mutant: the floor applies to special transactions. The same call to an ordinary address
/// shows what that rule would do.
#[test]
fn mutant_floor_applied_to_special_tx_is_detected() {
    assert_eq!(
        run(Address::repeat_byte(0x77), true),
        Err("GasPriceLessThanBasefee".into())
    );
}
