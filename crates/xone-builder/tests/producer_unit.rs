#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! The safety unit (it lives in `xone-consensus`; its tests are here because they need the in-memory fault store
//! of `xone-db`, which a lower-ranked crate may not depend on, `ci/layering.py`): the order persist, then sign; a retry writes nothing; a store failure stops it; a
//! restart keeps the record; the machine's observations are joined, never lowered.

use std::{cell::RefCell, rc::Rc};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use k256::ecdsa::SigningKey;
use xone_consensus::{
    unit::{Authorized, Backend, SafetyUnit, Sealer, UnitError},
    v2::safety::{BlockRef, Intent, Kind, Refusal, SafetyRecord},
};
use xone_db::safety::{Ack, Fault, Loaded, MemStore, SafetyStore, StoreError};
use xone_primitives::{AlgorithmId, Signature};

type Log = Rc<RefCell<Vec<&'static str>>>;

/// A store that shares its state with the test and logs each write.
#[derive(Clone)]
struct Shared(Rc<RefCell<MemStore>>, Log);

impl SafetyStore for Shared {
    fn load(&mut self) -> Result<Option<Loaded>, StoreError> {
        self.0.borrow_mut().load()
    }
    fn persist_intent(&mut self, i: &[u8], r: &[u8]) -> Result<Ack, StoreError> {
        let out = self.0.borrow_mut().persist_intent(i, r);
        self.1.borrow_mut().push(if out.is_ok() {
            "persisted"
        } else {
            "write failed"
        });
        out
    }
}

/// A throwaway key that logs each signature.
struct Key(SigningKey, Log);

impl Backend for Key {
    fn address(&self) -> Address {
        let p = self.0.verifying_key().to_encoded_point(false);
        Address::from_slice(&keccak256(&p.as_bytes()[1..])[12..])
    }
    fn sign(&self, a: &Authorized<'_>) -> Result<Signature, UnitError> {
        self.1.borrow_mut().push("signed");
        let (s, id) = self
            .0
            .sign_prehash_recoverable(a.digest().as_slice())
            .unwrap();
        let mut b = s.to_bytes().to_vec();
        b.push(id.to_byte());
        Ok(Signature {
            algorithm: AlgorithmId::Secp256k1Keccak,
            bytes: Bytes::from(b),
        })
    }
}

fn key(log: &Log) -> Key {
    Key(
        SigningKey::from_slice(&keccak256(b"throwaway unit key")[..]).unwrap(),
        log.clone(),
    )
}

fn d(n: u8) -> B256 {
    B256::with_last_byte(n)
}

fn open() -> (SafetyUnit<Shared, Key>, Shared, Log) {
    let log: Log = Rc::default();
    let store = Shared(Rc::default(), log.clone());
    let unit = SafetyUnit::open_or_init(store.clone(), key(&log), 0).unwrap();
    log.borrow_mut().clear();
    (unit, store, log)
}

#[test]
fn the_record_is_written_before_the_signature() {
    let (mut u, _, log) = open();
    u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
    assert_eq!(*log.borrow(), ["persisted", "signed"]);
    assert_eq!(u.signed_up_to(Kind::Propose), 5);
}

#[test]
fn a_retry_writes_nothing_and_signs_the_same_bytes() {
    let (mut u, _, log) = open();
    let a = u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
    let b = u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
    assert_eq!(a, b);
    assert_eq!(*log.borrow(), ["persisted", "signed", "signed"]);
}

#[test]
fn a_round_is_never_proposed_twice() {
    let (mut u, _, log) = open();
    u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
    log.borrow_mut().clear();
    for i in [
        Intent::new(Kind::Propose, 5, d(2)),
        Intent::new(Kind::Propose, 4, d(3)),
    ] {
        assert!(matches!(u.sign(i), Err(UnitError::Refused(_))));
    }
    assert!(
        log.borrow().is_empty(),
        "a refusal writes and signs nothing"
    );
}

#[test]
fn a_restart_keeps_the_mined_round() {
    let (mut u, store, log) = open();
    u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
    drop(u);
    store.0.borrow_mut().power_cycle(false);
    let mut u = SafetyUnit::open(store, key(&log), 0).unwrap();
    assert_eq!(u.signed_up_to(Kind::Propose), 5);
    let e = u.sign(Intent::new(Kind::Propose, 5, d(2))).unwrap_err();
    assert!(matches!(e, UnitError::Refused(Refusal::Conflict { .. })));
    u.sign(Intent::new(Kind::Propose, 6, d(2))).unwrap();
}

#[test]
fn a_failed_write_signs_nothing_and_stops_the_unit() {
    for fault in [
        Fault::BeforeWrite,
        Fault::AfterWrite,
        Fault::FailSync,
        Fault::AfterSync,
    ] {
        let (mut u, store, log) = open();
        store.0.borrow_mut().arm(fault);
        assert!(matches!(
            u.sign(Intent::new(Kind::Propose, 5, d(1))),
            Err(UnitError::Store(_))
        ));
        assert_eq!(
            *log.borrow(),
            ["write failed"],
            "{fault:?}: nothing was signed"
        );
        assert!(matches!(
            u.sign(Intent::new(Kind::Propose, 6, d(1))),
            Err(UnitError::Stopped)
        ));
        assert!(matches!(u.sign_tx(&d(9)), Err(UnitError::Stopped)));
    }
}

#[test]
fn a_closed_fence_signs_nothing() {
    let log: Log = Rc::default();
    let store = Shared(Rc::default(), log.clone());
    let mut u = SafetyUnit::open_or_init(store, key(&log), 3).unwrap();
    log.borrow_mut().clear();
    assert!(matches!(
        u.sign(Intent::new(Kind::Propose, 5, d(1))),
        Err(UnitError::FenceClosed)
    ));
    for _ in 0..3 {
        u.fence_mut().tick();
    }
    u.sign(Intent::new(Kind::Propose, 5, d(1))).unwrap();
}

#[test]
fn a_record_of_another_key_is_not_adopted() {
    let log: Log = Rc::default();
    let mut store = Shared(Rc::default(), log.clone());
    SafetyUnit::<Shared, Key>::init(&mut store, Address::repeat_byte(9)).unwrap();
    assert!(matches!(
        SafetyUnit::open(store, key(&log), 0),
        Err(UnitError::Identity { .. })
    ));
}

#[test]
fn an_uninitialised_store_is_refused() {
    let log: Log = Rc::default();
    let store = Shared(Rc::default(), log.clone());
    assert!(matches!(
        SafetyUnit::open(store, key(&log), 0),
        Err(UnitError::NotInitialised)
    ));
}

#[test]
fn a_sign_tx_does_not_change_the_record() {
    let (mut u, store, log) = open();
    let before = u.record().clone();
    u.sign_tx(&d(7)).unwrap();
    assert_eq!(*u.record(), before);
    assert_eq!(*log.borrow(), ["signed"]);
    assert_eq!(store.0.borrow().history_len(), 1, "only the init write");
}

#[test]
fn the_machine_record_is_joined_and_the_proposal_survives_its_later_writes() {
    let (mut u, _, _) = open();
    u.sign(Intent::new(Kind::Propose, 9, d(1))).unwrap();
    // the machine never saw the proposal; its record holds a QC and a vote
    let mut m = SafetyRecord::new(u.record().identity);
    m.observe_qc(
        BlockRef {
            hash: d(2),
            round: 8,
            number: 80,
        },
        None,
    );
    u.sign_with(Intent::vote(10, d(3), 8, true), Some(&m))
        .unwrap();
    let r = u.record();
    assert_eq!(r.signed[2].round, 9, "the proposal is still there");
    assert_eq!((r.highest_qc.round, r.highest_voted_round), (8, 10));
}

#[test]
fn a_v1_height_and_a_v2_round_do_not_bound_each_other() {
    let (mut u, _, _) = open();
    u.sign(Intent::new(Kind::SealV1, 2700, d(1))).unwrap();
    u.sign(Intent::new(Kind::Propose, 1, d(2))).unwrap();
    u.cosign_v1(2701, d(3)).unwrap();
    assert!(u.cosign_v1(2701, d(4)).is_err());
}
