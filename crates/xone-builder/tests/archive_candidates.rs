#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the vectors are trusted inputs and a panic is the failure signal"
)]
//! XDC special-transaction admission against the Apothem and net2040 archives: the candidate pool read from the
//! 0x88 words of a recent epoch's gap block equals the three lists the node reports (masternodes, penalised,
//! standby); real sign transactions of masternodes and of penalised candidates are admitted and promoted; the
//! same transactions without their sender in the pool are refused. Built by `build/special.py`.
mod common;

use alloy_primitives::{Address, Bytes, B256, U256};
use common::{account, vectors, MapState};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc};
use xone_builder::txpool::{
    special::{Authority, SIGN_SELECTOR},
    Config, Error, Pool, PoolTx, Status,
};
use xone_chainspec::{load_named, ChainSpec, Network};
use xone_consensus::v2::gap_snapshot;
use xone_executor::tx::Tx;
use xone_state::{Account, StateRead};

#[derive(Deserialize)]
struct Lists {
    masternodes: Vec<Address>,
    penalised: Vec<Address>,
    standby: Vec<Address>,
}

#[derive(Deserialize)]
struct Sign {
    class: String,
    block: u64,
    hash: B256,
    from: Address,
    nonce: u64,
    balance: String,
    head_gas_limit: u64,
    raw: Bytes,
}

#[derive(Deserialize)]
struct Vec_ {
    network: String,
    chain_id: u64,
    head: u64,
    gap: u64,
    epoch: u64,
    words: Vec<(B256, B256)>,
    lists: Lists,
    sign_txs: Vec<Sign>,
}

struct Words(std::collections::BTreeMap<B256, U256>);
impl StateRead for Words {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, _: Address, slot: B256) -> U256 {
        self.0.get(&slot).copied().unwrap_or_default()
    }
}

fn spec(net: &str) -> ChainSpec {
    match net {
        "apothem" => Network::Apothem.spec(),
        _ => load_named(
            "net2040",
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/vectors/consensus/net2040.forks.json"
            ))
            .unwrap(),
        )
        .unwrap(),
    }
}

fn synthetic_sign(from: Address, block: u64) -> Arc<PoolTx> {
    let mut data = SIGN_SELECTOR.to_vec();
    data.extend_from_slice(&U256::from(block).to_be_bytes::<32>());
    data.extend_from_slice(&[9u8; 32]);
    let mut h = [0u8; 32];
    h[..20].copy_from_slice(from.as_slice());
    Arc::new(PoolTx {
        hash: B256::from(h),
        size: 190,
        tx: Tx {
            ty: 0,
            chain_id: None,
            nonce: 0,
            gas_price: 0,
            priority_fee: None,
            gas: 200_000,
            to: Some(xone_evm::route::BLOCK_SIGNERS),
            value: U256::ZERO,
            data: Bytes::from(data),
            access_list: vec![],
            from,
        },
    })
}

fn check(file: &str) -> (usize, usize, usize) {
    let v: Vec_ = serde_json::from_slice(&std::fs::read(vectors(file)).unwrap()).unwrap();
    let spec = spec(&v.network);
    assert_eq!(spec.chain_id, v.chain_id);
    // 1. The pool computed from the contract words is the union of the node's three lists.
    assert_eq!(
        gap_snapshot::gap_number(v.head, v.epoch, spec.xdpos.gap).unwrap(),
        v.gap
    );
    let words = Words(
        v.words
            .iter()
            .map(|(k, w)| (*k, U256::from_be_bytes(w.0)))
            .collect(),
    );
    let snap = gap_snapshot::build(&words, v.gap).unwrap();
    let pool: BTreeSet<Address> = snap.candidates.iter().copied().collect();
    let rpc: BTreeSet<Address> = v
        .lists
        .masternodes
        .iter()
        .chain(&v.lists.penalised)
        .chain(&v.lists.standby)
        .copied()
        .collect();
    assert_eq!(
        pool, rpc,
        "{}: candidate pool from 0x88 equals masternodes + penalised + standby",
        v.network
    );
    assert!(
        !v.lists.penalised.is_empty(),
        "{}: the sample has penalised candidates",
        v.network
    );
    let auth = Authority::candidates(snap.candidates.iter().copied());
    let free = |a: Address| MapState([(a, account(0, U256::from(1u64), &[]))].into());
    let cfg = Config::default();
    let (mut real, mut penalised_real) = (0, 0);
    // 2. Real sign transactions: admitted and promoted; refused once the sender is not a candidate.
    for t in &v.sign_txs {
        let mut st = MapState::default();
        st.0.insert(
            t.from,
            account(t.nonce, U256::from_str_radix(&t.balance, 10).unwrap(), &[]),
        );
        let mut p = Pool::new(
            spec.clone(),
            &cfg,
            auth.clone(),
            t.block - 1,
            t.head_gas_limit,
        )
        .unwrap();
        let raw = PoolTx::decode(&t.raw, spec.chain_id).unwrap();
        assert_eq!(
            (raw.hash, raw.from()),
            (t.hash, t.from),
            "rebuilt transaction is the archive's"
        );
        assert_eq!(
            p.add_batch(&st, 1_700_000_000, std::slice::from_ref(&raw))
                .remove(0),
            Ok(()),
            "{} sign tx of a {}",
            t.hash,
            t.class
        );
        assert_eq!(p.status(&raw.hash), Status::Pending, "promoted");
        assert_eq!(raw.tx.gas_price, 0, "below any floor");
        let without: Vec<Address> = snap
            .candidates
            .iter()
            .copied()
            .filter(|a| *a != t.from)
            .collect();
        let mut q = Pool::new(
            spec.clone(),
            &cfg,
            Authority::candidates(without),
            t.block - 1,
            t.head_gas_limit,
        )
        .unwrap();
        let e = q
            .add_batch(&st, 1_700_000_000, std::slice::from_ref(&raw))
            .remove(0)
            .unwrap_err();
        assert!(
            matches!(e, Error::SpecialTxNotFromSigner(a) if a == t.from),
            "{e}"
        );
        real += 1;
        penalised_real += usize::from(t.class == "penalised");
    }
    // 3. Every penalised candidate is authorised, every address outside the pool is not.
    let mut p = Pool::new(spec.clone(), &cfg, auth, v.head, 50_000_000).unwrap();
    let mut st = MapState::default();
    for a in &v.lists.penalised {
        st.0 = free(*a).0;
        let t = synthetic_sign(*a, v.head - 5);
        assert_eq!(
            p.add_batch(&st, 1_700_000_000, std::slice::from_ref(&t))
                .remove(0),
            Ok(()),
            "penalised {a}"
        );
        assert_eq!(p.status(&t.hash), Status::Pending);
    }
    for i in 0..20u8 {
        let a = Address::repeat_byte(0xe0 + i % 16);
        assert!(!pool.contains(&a));
        st.0 = free(a).0;
        let r = p
            .add_batch(&st, 1_700_000_000, &[synthetic_sign(a, v.head - 5)])
            .remove(0);
        assert!(
            matches!(r, Err(Error::SpecialTxNotFromSigner(_))),
            "outsider {a}: {r:?}"
        );
    }
    (real, penalised_real, v.lists.penalised.len())
}

#[test]
fn apothem_recent_epoch() {
    let (real, pen, lists) = check("special-apothem.json");
    eprintln!("apothem: {real} real sign txs ({pen} from penalised candidates) admitted and promoted, {lists} penalised candidates admitted, outsiders refused");
    assert!(pen >= 1, "a real sign transaction of a penalised candidate");
}

#[test]
fn net2040_recent_epoch() {
    let (real, _, lists) = check("special-net2040.json");
    eprintln!("net2040: {lists} penalised candidates authorised (the pool cannot open at an Amsterdam head); real sign txs recovered and authorised: {real}");
}
