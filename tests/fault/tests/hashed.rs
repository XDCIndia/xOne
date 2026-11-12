#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The hashed-key state tables (ADR-005 amendment 2026-11-12): a schema 1 datadir (plain tables,
//! changesets keyed by address) is migrated on open, in one transaction, to schema 2; the state, the
//! state root and every retained older state read back the same. A newer schema is refused.

mod support;
use alloy_primitives::B256;
use support::{crash::*, *};
use xone_db::{
    fault::Pick,
    mdbx::{Env, Wr, T},
};
use xone_fault::Scratch;
use xone_sync::chain::Resumed;

const LABELS: [&str; 6] = ["m1", "m2", "m3", "m4", "m5", "m6"];

fn built(p: &Parts) -> Dead {
    let sc = Scratch::new("hashed");
    let chain = p.chain(p.fresh(&sc));
    for l in LABELS {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    let d = take_any(chain.into_store());
    std::mem::forget(sc); // the Dead keeps the path; the OS temp dir is cleaned by the harness
    d
}

fn edit(path: &std::path::Path, f: impl Fn(&Wr)) {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let tx = env.write().unwrap();
    f(&tx);
    tx.commit().unwrap();
}

fn rows(path: &std::path::Path, t: T) -> Vec<(Vec<u8>, Vec<u8>)> {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let rd = env.read().unwrap();
    rd.range(t, &[], None).unwrap()
}

/// What an older build wrote: the hashed rows rewritten as plain rows (addresses from the preimage
/// table), the changesets keyed by address, no schema row and no preimage table.
fn downgrade(tx: &Wr) {
    let pre = |h: &[u8]| tx.get(T::Preimages, h).unwrap().expect("a preimage");
    for (k, v) in tx.range(T::HashedAccounts, &[], None).unwrap() {
        tx.put(T::PlainAccounts, &pre(&k), &v).unwrap();
        tx.del(T::HashedAccounts, &k).unwrap();
    }
    for (k, v) in tx.range(T::HashedStorages, &[], None).unwrap() {
        let key = [pre(&k[..32]), pre(&k[32..])].concat();
        tx.put(T::PlainStorage, &key, &v).unwrap();
        tx.del(T::HashedStorages, &k).unwrap();
    }
    for (k, v) in tx.range(T::AccountChangeSets, &[], None).unwrap() {
        tx.del(T::AccountChangeSets, &k).unwrap();
        tx.put(T::AccountChangeSets, &[&k[..8], &pre(&k[8..])].concat(), &v)
            .unwrap();
    }
    for (k, v) in tx.range(T::StorageChangeSets, &[], None).unwrap() {
        tx.del(T::StorageChangeSets, &k).unwrap();
        let key = [&k[..8], &pre(&k[8..40])[..], &pre(&k[40..])[..]].concat();
        tx.put(T::StorageChangeSets, &key, &v).unwrap();
    }
    tx.del_range(T::Preimages, &[], None).unwrap();
    tx.del(T::Meta, b"schema").unwrap();
}

#[test]
fn a_schema_1_datadir_migrates_on_open_and_keeps_every_retained_state() {
    let p = Parts::new(true, 0, LABELS, 4);
    let d = built(&p);
    let (sc, img) = d.image(Pick::Nothing);
    let mdbx = sc.path().join("chain.mdbx");
    let (accounts, slots) = (
        rows(&mdbx, T::HashedAccounts),
        rows(&mdbx, T::HashedStorages),
    );
    let (acs, scs) = (
        rows(&mdbx, T::AccountChangeSets),
        rows(&mdbx, T::StorageChangeSets),
    );
    assert!(!accounts.is_empty() && !slots.is_empty() && !acs.is_empty() && !scs.is_empty());
    assert!(rows(&mdbx, T::PlainAccounts).is_empty());
    assert_eq!(acs[0].0.len(), 8 + 32, "changeset keys are hashed");

    edit(&mdbx, downgrade);
    assert!(rows(&mdbx, T::HashedAccounts).is_empty());
    assert_eq!(rows(&mdbx, T::PlainAccounts).len(), accounts.len());
    assert_eq!(rows(&mdbx, T::AccountChangeSets)[0].0.len(), 8 + 20);

    let (row, res) = restart(&p, &sc, img);
    let _ = row;
    let (chain, resumed) = res.unwrap();
    assert!(matches!(resumed, Resumed::Head { .. }));
    for l in ["m6", "m5", "m3", "m1"] {
        assert_state_of(&chain, &p.t, l);
    }
    assert_eq!(chain.head().1, p.t.hash("m6"));
    drop(chain);

    // the migrated tables are exactly the ones the new build writes, byte for byte
    assert_eq!(rows(&mdbx, T::HashedAccounts), accounts);
    assert_eq!(rows(&mdbx, T::HashedStorages), slots);
    assert_eq!(rows(&mdbx, T::AccountChangeSets), acs);
    assert_eq!(rows(&mdbx, T::StorageChangeSets), scs);
    assert!(rows(&mdbx, T::PlainAccounts).is_empty());
    assert!(rows(&mdbx, T::PlainStorage).is_empty());
    assert!(!rows(&mdbx, T::Preimages).is_empty(), "preimages come back");
    assert_eq!(
        rows(&mdbx, T::Meta)
            .iter()
            .filter(|(k, _)| k == b"schema")
            .count(),
        1
    );
}

#[test]
fn a_newer_schema_is_refused_and_a_new_datadir_gets_schema_2() {
    let p = Parts::new(true, 0, LABELS, 4);
    let d = built(&p);
    let (sc, img) = d.image(Pick::Nothing);
    let mdbx = sc.path().join("chain.mdbx");
    let schema = rows(&mdbx, T::Meta)
        .into_iter()
        .find(|(k, _)| k == b"schema")
        .unwrap()
        .1;
    assert_eq!(schema, 2u64.to_le_bytes());
    edit(&mdbx, |tx| {
        tx.put(T::Meta, b"schema", &3u64.to_le_bytes()).unwrap()
    });
    let err = xone_sync::mdbx::MdbxStore::open(&mdbx, img, xone_fault::cfg(64, 4))
        .err()
        .expect("refused");
    assert!(err.to_string().contains("schema 3"), "{err}");
    let _ = B256::ZERO;
}
