#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Restart over the datadir (MDBX and segment files, ADR-005): the head comes back from the manifest,
//! the blocks above the newest state checkpoint are replayed and verified, and a damaged or foreign
//! datadir is refused, never opened at block 0.

mod common;

use std::path::Path;

use common::{parts, scratch, vectors};
use xone_chainspec::Network;
use xone_node::services::core::{Core, CoreError};

const N: usize = 12;

fn open(dir: &Path, net: Network, every: u64) -> Result<Core, CoreError> {
    Core::open(
        net.spec(),
        &net.genesis().unwrap(),
        &dir.join("xone"),
        every,
    )
}

fn fill(dir: &Path, every: u64) -> Core {
    let blocks = vectors(N);
    let core = open(dir, Network::Apothem, every).unwrap();
    for b in &blocks[1..] {
        let (h, body) = parts(b);
        core.import(h, body, xone_node::services::core::now())
            .unwrap();
    }
    assert_eq!(core.head(), (N as u64, blocks[N].header.hash()));
    core
}

#[test]
fn head_and_blocks_come_back() {
    let dir = scratch("restart");
    let want = vectors(N)[N].header.hash();
    let (receipts, txs) = {
        let core = fill(&dir, 5); // checkpoints at 5 and 10, none at 12 (no clean stop)
        let h = core.hist.read();
        (
            h.tip().receipts.clone(),
            h.blocks
                .iter()
                .map(|b| b.body.transactions.len())
                .sum::<usize>(),
        )
    };
    assert!(dir.join("xone/state/ckpt-00000010.json.gz").exists());
    assert!(!dir.join("xone/state/ckpt-00000012.json.gz").exists());

    // from the checkpoint of block 10, replaying 11 and 12
    let core = open(&dir, Network::Apothem, 5).unwrap();
    assert_eq!(core.head(), (N as u64, want));
    {
        let h = core.hist.read();
        assert_eq!(h.blocks.len(), N + 1);
        assert_eq!(h.tip().receipts, receipts);
        // the receipts segment gives back what was imported (the RPC reads old blocks from it)
        for b in h.blocks.iter().skip(1) {
            let n = b.header.number;
            let (_, _, stored) = core
                .chain
                .with_store(|s| s.canonical_block(n))
                .unwrap()
                .unwrap();
            assert_eq!(stored, b.receipts);
        }
        assert_eq!(
            h.blocks
                .iter()
                .map(|b| b.body.transactions.len())
                .sum::<usize>(),
            txs
        );
    }
    drop(core);

    // a checkpoint that does not parse is skipped, then the replay runs from genesis
    std::fs::write(dir.join("xone/state/ckpt-00000010.json.gz"), b"not gzip").unwrap();
    std::fs::remove_file(dir.join("xone/state/ckpt-00000005.json.gz")).unwrap();
    let core = open(&dir, Network::Apothem, 0).unwrap();
    assert_eq!(core.head(), (N as u64, want));
    // and the node goes on from there: the next stored block is not written twice
    let b13 = &vectors(N + 1)[N + 1];
    let (h, body) = parts(b13);
    core.import(h, body, xone_node::services::core::now())
        .unwrap();
    drop(core);
    assert_eq!(
        open(&dir, Network::Apothem, 0).unwrap().head().0,
        N as u64 + 1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_torn_tail_is_cut_and_damage_is_refused() {
    let dir = scratch("damage");
    drop(fill(&dir, 0));
    let seg = |n: &str| dir.join("xone/static").join(n).join("0000000000.seg");
    // bytes after the committed length (a torn append) are ignored and cut
    let mut v = std::fs::read(seg("headers")).unwrap();
    let good = v.len();
    v.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 1, 2, 3]);
    std::fs::write(seg("headers"), &v).unwrap();
    let core = open(&dir, Network::Apothem, 0).unwrap();
    assert_eq!(core.head().0, N as u64);
    assert_eq!(
        std::fs::metadata(seg("headers")).unwrap().len() as usize,
        good
    );
    drop(core);

    // a flipped byte inside a committed frame is refused
    let mut v = std::fs::read(seg("bodies")).unwrap();
    let mid = v.len() / 2;
    v[mid] ^= 1;
    std::fs::write(seg("bodies"), &v).unwrap();
    let e = open(&dir, Network::Apothem, 0).err().unwrap();
    assert!(matches!(e, CoreError::File(_)), "{e}");
    v[mid] ^= 1;
    std::fs::write(seg("bodies"), &v).unwrap();
    assert!(open(&dir, Network::Apothem, 0).is_ok());

    // a segment shorter than the manifest says is refused
    let v = std::fs::read(seg("receipts")).unwrap();
    std::fs::write(seg("receipts"), &v[..v.len() - 1]).unwrap();
    let e = open(&dir, Network::Apothem, 0).err().unwrap();
    assert!(e.to_string().contains("SegmentShort"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn another_chain_is_refused() {
    let dir = scratch("foreign");
    drop(fill(&dir, 0));
    let e = open(&dir, Network::Devnet, 0).err().unwrap();
    assert!(
        e.to_string().contains("does not start at this genesis"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A second opener of a datadir that a live Core holds is refused before it touches a file.
#[test]
fn a_second_opener_of_a_live_datadir_is_refused() {
    let dir = scratch("lock");
    let first = open(&dir, Network::Apothem, 0).unwrap();
    let e = open(&dir, Network::Apothem, 0).err().expect("second open");
    assert!(e.to_string().contains("in use"), "{e}");
    drop(first);
    open(&dir, Network::Apothem, 0).unwrap();
}

/// Past the memory window the history keeps no body and no receipts of a block (memory follows the
/// window, not the chain); they are read back from the store, and a restart from a checkpoint still
/// opens at the head with the same split.
#[test]
fn memory_follows_the_window() {
    use xone_node::services::hist::MEMORY_WINDOW;
    let dir = scratch("window");
    let n = 3 * MEMORY_WINDOW as usize;
    let blocks = vectors(n);
    let old = (1..n - MEMORY_WINDOW as usize)
        .find(|i| !blocks[*i].transactions.is_empty())
        .expect("a block with a transaction below the window");
    let check = |core: &Core| {
        assert_eq!(core.head(), (n as u64, blocks[n].header.hash()));
        {
            let h = core.hist.read();
            let s = h.at(old as u64).unwrap();
            assert!(s.body.transactions.is_empty() && s.receipts.is_empty());
            let tip = h.at(n as u64).unwrap();
            assert_eq!(tip.body.transactions.len(), blocks[n].transactions.len());
        }
        let (body, receipts) = core.records(old as u64, blocks[old].header.hash()).unwrap();
        assert_eq!(body.transactions, parts(&blocks[old]).1.transactions);
        assert_eq!(receipts.len(), body.transactions.len());
    };
    let core = open(&dir, Network::Apothem, 100).unwrap();
    for b in &blocks[1..] {
        let (h, body) = parts(b);
        core.import(h, body, xone_node::services::core::now())
            .unwrap();
    }
    check(&core);
    // a full node keeps the newest two checkpoints: an older one is outside the unwind window
    let ck = |n: u64| dir.join(format!("xone/state/ckpt-{n:08}.json.gz")).exists();
    assert!(!ck(100) && ck(200) && ck(300));
    drop(core);
    check(&open(&dir, Network::Apothem, 100).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The history holds a window, not the chain: below it headers, hashes, numbers and transaction
/// locations come from the store (`CanonicalHeaders`, `HeaderNumbers`, the header segment,
/// `TxLookup`), a restart rebuilds the window from the store and the checkpoint without reading the
/// bodies below the memory window, an unwind takes the `TxLookup` rows of the blocks it drops, and a
/// consensus read below the window is refused, never answered with an empty list.
#[test]
fn history_is_a_window_over_the_store() {
    use alloy_primitives::keccak256;
    let dir = scratch("window-store");
    let blocks = vectors(1800);
    let txs: Vec<(usize, alloy_primitives::B256)> = (1..1800)
        .filter_map(|i| Some((i, keccak256(parts(&blocks[i]).1.transactions.first()?))))
        .collect();
    let old = *txs
        .iter()
        .find(|(i, _)| *i < 600)
        .expect("a transaction below 600");
    let above = *txs
        .iter()
        .find(|(i, _)| (1101..=1200).contains(i))
        .expect("one in 1101..=1200");
    let check = |core: &Core, base_at_least: u64| {
        let h = core.hist.read();
        assert_eq!(h.tip_number(), 1200);
        assert!(
            h.base >= base_at_least && h.base > old.0 as u64,
            "{}",
            h.base
        );
        assert!(h.blocks.len() as u64 <= h.window);
        let (n, want) = (old.0 as u64, blocks[old.0].header.hash());
        assert!(h.at(n).is_none());
        assert_eq!(h.header_at(n).unwrap().hash(), want);
        assert_eq!((h.hash_at(n), h.number_of(want)), (Some(want), Some(n)));
        assert_eq!(h.tx(old.1), Some((n, 0)));
        assert_eq!(h.tx(above.1), Some((above.0 as u64, 0)));
        assert!(h.hash_at(1201).is_none() && h.header_at(1201).is_none());
        drop(h);
        let (body, receipts) = core.records(n, want).unwrap();
        assert_eq!(body.transactions, parts(&blocks[old.0]).1.transactions);
        assert_eq!(receipts.len(), body.transactions.len());
    };

    let core = open(&dir, Network::Apothem, 500).unwrap();
    core.hist.inner.write().unwrap().window = 300;
    for b in &blocks[1..=1200] {
        let (h, body) = parts(b);
        core.import(h, body, xone_node::services::core::now())
            .unwrap();
    }
    assert_eq!(core.hist.read().blocks.len(), 300);
    check(&core, 901);
    drop(core);

    // from the checkpoint of 1,000: its window (701..=1,000) from the store, 1,001..=1,200 replayed
    let core = open(&dir, Network::Apothem, 500).unwrap();
    check(&core, 701);
    // the V1 reward of 1,800 reads the sign transactions of 1..=1,799 (geth `GetRewardForCheckpoint`),
    // and those below the window are not held: the block is refused, not paid from an empty list
    for b in &blocks[1201..1800] {
        let (h, body) = parts(b);
        core.import(h, body, xone_node::services::core::now())
            .unwrap();
    }
    let (h, body) = parts(&blocks[1800]);
    let e = core
        .import(h, body, xone_node::services::core::now())
        .expect_err("block 1,800 needs signs below the window");
    let e = e.to_string();
    assert!(
        e.contains("the body of block 700") && e.contains("is not available"),
        "{e}"
    );

    // an unwind deletes the transaction index of the blocks it drops
    core.chain.set_head(1100).unwrap();
    let src = core.hist.read();
    let src = src.src.as_ref().unwrap();
    assert_eq!(src.tx(above.1).unwrap(), None);
    assert_eq!(src.tx(old.1).unwrap(), Some((old.0 as u64, 0)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A datadir of a build without the history tables is refused with its typed error.
#[test]
fn a_datadir_without_history_tables_is_refused() {
    let dir = scratch("no-history");
    drop(fill(&dir, 0));
    let names: Vec<_> = xone_db::mdbx::T::ALL.iter().map(|t| t.name()).collect();
    {
        let env = xone_db::mdbx::Env::open(&dir.join("xone/chain.mdbx"), &names).unwrap();
        let tx = env.write().unwrap();
        tx.del(xone_db::mdbx::T::Meta, b"history").unwrap();
        tx.commit().unwrap();
    }
    let e = open(&dir, Network::Apothem, 0).err().unwrap();
    assert!(matches!(e, CoreError::NoHistoryTables), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}
