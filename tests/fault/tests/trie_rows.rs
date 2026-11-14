#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The trie rows of `--state.disk` (issue #145, part 2): written in the block's own transaction with
//! the `trieroot` marker, equal after an unwind to what a rebuild from the flat tables gives, used as
//! they are on a restart when the marker is the head's root, and rebuilt (and checked against the
//! head header) when the marker is missing or wrong or a row is damaged.

mod support;
use support::{crash::*, *};
use xone_db::{
    fault::{MemDir, Pick},
    mdbx::{Env, Wr, T},
};
use xone_fault::{anchored, cfg, Scratch};
use xone_sync::{chain::Chain, mdbx::Config};

const LABELS: [&str; 6] = ["m1", "m2", "m3", "m4", "m5", "m6"];

fn dcfg(p: &Parts) -> Config {
    let mut c = cfg(p.retention, p.bpf);
    c.disk_state = true;
    c
}

fn open(p: &Parts, sc: &Scratch, img: MemDir) -> MChain {
    let store = xone_sync::mdbx::MdbxStore::open(&sc.path().join("chain.mdbx"), img, dcfg(p))
        .unwrap_or_else(|e| panic!("open: {e}"));
    Chain::open(p.spec.clone(), store, p.exec.clone(), accept as Accept)
        .unwrap_or_else(|e| panic!("chain open: {e:?}"))
        .0
}

fn edit(path: &std::path::Path, f: impl Fn(&Wr)) {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let tx = env.write().unwrap();
    f(&tx);
    tx.commit().unwrap();
}

fn trie_rows(path: &std::path::Path) -> xone_sync::mdbx::TrieRows {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    let env = Env::open(path, &names).unwrap();
    let rd = env.read().unwrap();
    (
        rd.range(T::TrieNodes, &[], None).unwrap(),
        rd.get(T::Meta, b"trieroot").unwrap(),
    )
}

/// The disk of the store as it stands, for a restart (the store is dropped).
fn stopped(chain: MChain) -> (Scratch, MemDir) {
    take_any(chain.into_store()).image(Pick::Nothing)
}

#[test]
fn trie_rows_follow_blocks_and_unwinds_and_are_rebuilt_when_not_trusted() {
    let p = Parts::new(true, 0, LABELS, 4);
    let sc = Scratch::new("trie-rows");
    let g = p.t.blocks["s0"].block.header.clone();
    let store = anchored(&sc, MemDir::new(), g, Default::default(), dcfg(&p));
    let chain = p.chain(store);
    for l in LABELS {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    let root = p.t.blocks["m6"].block.header.state_root;
    let (sc1, img) = stopped(chain);
    let mdbx = sc1.path().join("chain.mdbx");
    let (rows, marker) = trie_rows(&mdbx);
    assert!(!rows.is_empty(), "the blocks wrote trie rows");
    assert_eq!(marker.as_deref(), Some(root.as_slice()));

    // a restart with the marker at the head's root uses the rows as they are
    let chain = open(&p, &sc1, img);
    assert_eq!(chain.head().1, p.t.hash("m6"));
    assert_eq!(chain.with_store(|s| s.state_root()), root);
    let (sc2, img) = stopped(chain);
    assert_eq!(
        trie_rows(&sc2.path().join("chain.mdbx")),
        (rows.clone(), marker.clone())
    );

    // no marker, a wrong marker, a damaged row: rebuilt from the flat tables, same rows, same root
    let mdbx2 = sc2.path().join("chain.mdbx");
    edit(&mdbx2, |tx| tx.del(T::Meta, b"trieroot").unwrap());
    let chain = open(&p, &sc2, img);
    assert_eq!(chain.with_store(|s| s.state_root()), root);
    let (sc3, img) = stopped(chain);
    let mdbx3 = sc3.path().join("chain.mdbx");
    assert_eq!(
        trie_rows(&mdbx3),
        (rows.clone(), marker.clone()),
        "rebuild equals the rows the blocks wrote"
    );

    edit(&mdbx3, |tx| {
        tx.put(T::Meta, b"trieroot", &[7u8; 32]).unwrap();
        let (k, v) = tx.range(T::TrieNodes, &[], None).unwrap().remove(1);
        let mut v = v;
        v[0] ^= 1;
        tx.put(T::TrieNodes, &k, &v).unwrap();
    });
    let chain = open(&p, &sc3, img);
    let (sc4, img) = stopped(chain);
    assert_eq!(trie_rows(&sc4.path().join("chain.mdbx")), (rows, marker));

    // an unwind writes the rows of the older state in its own transaction
    let chain = open(&p, &sc4, img);
    chain.set_head(3).unwrap();
    let root3 = p.t.blocks["m3"].block.header.state_root;
    assert_eq!(chain.with_store(|s| s.state_root()), root3);
    let (sc5, img) = stopped(chain);
    let mdbx5 = sc5.path().join("chain.mdbx");
    let (rows3, marker3) = trie_rows(&mdbx5);
    assert_eq!(marker3.as_deref(), Some(root3.as_slice()));
    edit(&mdbx5, |tx| tx.del(T::Meta, b"trieroot").unwrap());
    let chain = open(&p, &sc5, img);
    let (sc6, _img) = stopped(chain);
    assert_eq!(
        trie_rows(&sc6.path().join("chain.mdbx")),
        (rows3, marker3),
        "the rows after an unwind are those a rebuild gives"
    );
}
