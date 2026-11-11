//! STATICCALL touches the callee, and EIP-161 then deletes it if empty, only from the Istanbul
//! rules (geth `core/vm/evm.go:513`). The archive cannot show this: its `prestateTracer` drops
//! empty accounts. So the expectation comes from the oracle (`oracle/touch.go`), which runs the
//! same contract through geth-XDC at the blocks around `TIPXDCXCancellationFee`.

use alloy_primitives::{Address, Bytes, U256};
use revm::{
    context::TxEnv,
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::Handler,
    primitives::TxKind,
    state::{AccountInfo, Bytecode},
};
use serde::Deserialize;
use std::str::FromStr;
use xone_chainspec::Network;
use xone_evm::{
    handler::{build_evm, XdcHandler},
    spec::{XdcBlock, BASEFEE_OPCODE_WITHOUT_HEADER_FEE},
};

#[derive(Deserialize)]
struct File {
    pin: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    network: String,
    number: u64,
    sender: String,
    contract: String,
    code: String,
    target: String,
    #[serde(rename = "existsAfter")]
    exists_after: bool,
}

#[test]
fn static_call_touch_equals_geth_around_the_istanbul_rules() {
    let path = format!(
        "{}/../../conformance/vectors/evm/static_call_touch.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: File = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(f.pin, include_str!("../../../conformance/PIN").trim());
    assert_eq!(f.cases.len(), 6);
    for c in f.cases {
        let a = |s: &str| Address::from_str(s).unwrap();
        let (sender, contract, target) = (a(&c.sender), a(&c.contract), a(&c.target));
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(10u64).pow(U256::from(21)),
                ..Default::default()
            },
        );
        let code = Bytecode::new_raw(Bytes::from(alloy_primitives::hex::decode(&c.code).unwrap()));
        db.insert_account_info(
            contract,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
        db.insert_account_info(target, AccountInfo::default());

        let spec = Network::from_str(&c.network).unwrap().spec();
        let block = XdcBlock {
            number: c.number,
            gas_limit: 30_000_000,
            difficulty: U256::from(1),
            base_fee: Some(BASEFEE_OPCODE_WITHOUT_HEADER_FEE),
            ..Default::default()
        };
        let mut evm = build_evm(db, &spec, &block, None).unwrap();
        evm.ctx.tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(contract),
            gas_limit: 200_000,
            gas_price: u128::from(BASEFEE_OPCODE_WITHOUT_HEADER_FEE),
            chain_id: None,
            ..Default::default()
        };
        let res = XdcHandler::default().run(&mut evm).unwrap();
        assert!(res.is_success(), "{} {}", c.network, c.number);
        let state = evm.ctx.journaled_state.finalize();
        let exists = state
            .get(&target)
            .is_none_or(|t| !(t.is_touched() && t.is_empty()));
        assert_eq!(exists, c.exists_after, "{} block {}", c.network, c.number);
    }
}
