#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)]
//! `MdbxStore` against the in-memory model: the same operations leave the same heads, indexes,
//! states and side blocks, and a restart on the durable bytes alone gives them back.

mod support;
use support::*;
use xone_db::fault::Pick;
use xone_fault::Scratch;
use xone_sync::{
    chain::{Chain, Resumed},
    store::Store,
    testkit::MemStore,
};

fn labels() -> Vec<String> {
    ["s1", "s2", "s3", "m4", "m5", "m6", "a4", "a5", "a6", "a7"]
        .map(String::from)
        .to_vec()
}

#[test]
fn same_operations_same_results_and_a_restart_gives_them_back() {
    for v2 in [true, false] {
        let p = Parts::new(v2, 3, labels(), 4);
        let scratch = Scratch::new("equiv");
        let mem = Chain::new(
            p.spec.clone(),
            MemStore::with_anchor(
                p.t.blocks["s0"].block.header.clone(),
                Default::default(),
                p.retention,
                p.bpf,
            ),
            p.exec.clone(),
            accept as Accept,
        );
        let mdbx = p.chain(p.fresh(&scratch));
        // a main chain, a side branch that overtakes it (reorg), a late sibling
        for l in ["s1", "s2", "s3", "m4", "m5", "a4", "a5", "a6", "m6", "a7"] {
            let a = mem.insert(p.t.block(l), 0).map_err(|e| e.to_string());
            let b = mdbx.insert(p.t.block(l), 0).map_err(|e| e.to_string());
            assert_eq!(a, b, "insert {l}");
            assert_eq!(mem.head(), mdbx.head(), "head after {l}");
            assert_eq!(mem.committed(), mdbx.committed(), "floor after {l}");
        }
        // set-head drops blocks, then they are imported again
        for n in [5, 3] {
            assert_eq!(
                mem.set_head(n).map_err(|e| e.to_string()),
                mdbx.set_head(n).map_err(|e| e.to_string()),
                "set-head {n}"
            );
            assert_eq!(mem.head(), mdbx.head());
        }
        let (ms, ds) = (mem.into_store(), mdbx.into_store());
        assert_eq!(ms.canonical_numbers(), ds.canonical_numbers());
        assert_eq!(ms.side_hashes(), ds.side_hashes());
        assert_eq!(ms.flat_root(), ds.flat_root());
        for n in 0..=ds.head().0 {
            let h = ds.canonical_hash(n).unwrap();
            assert_eq!(ms.header(h).unwrap(), ds.header(h).unwrap(), "header {n}");
            let (a, b) = (ms.record(h).unwrap(), ds.record(h).unwrap());
            assert_eq!((a.block.hash(), a.receipts), (b.block.hash(), b.receipts));
        }
        let chain = p.chain(ds);
        let label = consistent(&chain, &p.t);
        // the durable bytes alone (power loss with every fsync honoured) are the whole chain
        let (c2, r) = p
            .reopen(&scratch, chain.into_store(), Pick::Nothing)
            .unwrap();
        assert!(matches!(r, Resumed::Head { repaired: 0, .. }), "{r:?}");
        assert_eq!(consistent(&c2, &p.t), label);
        let store = c2.into_store();
        assert_eq!(store.side_hashes(), ms.side_hashes());
        assert_eq!(store.flat_root(), ms.flat_root());
    }
}
