#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Restart over the datadir files: the head comes back from `head.json`, the blocks above the newest
//! state checkpoint are replayed and verified, and a damaged or foreign datadir is refused, never
//! opened at block 0.

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
        for b in &h.blocks[1..] {
            let n = b.header.number;
            assert_eq!(
                core.chain.with_store(|s| s.receipts(n)).unwrap(),
                b.receipts
            );
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
    let seg = |n: &str| dir.join("xone/static").join(n);
    // bytes after the committed length (a torn append) are ignored and cut
    let mut v = std::fs::read(seg("headers.seg")).unwrap();
    let good = v.len();
    v.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 1, 2, 3]);
    std::fs::write(seg("headers.seg"), &v).unwrap();
    let core = open(&dir, Network::Apothem, 0).unwrap();
    assert_eq!(core.head().0, N as u64);
    assert_eq!(
        std::fs::metadata(seg("headers.seg")).unwrap().len() as usize,
        good
    );
    drop(core);

    // a flipped byte inside a committed frame is refused
    let mut v = std::fs::read(seg("bodies.seg")).unwrap();
    v[40] ^= 1;
    std::fs::write(seg("bodies.seg"), &v).unwrap();
    let e = open(&dir, Network::Apothem, 0).err().unwrap();
    assert!(matches!(e, CoreError::File(_)), "{e}");
    v[40] ^= 1;
    std::fs::write(seg("bodies.seg"), &v).unwrap();
    assert!(open(&dir, Network::Apothem, 0).is_ok());

    // a segment shorter than head.json says is refused
    let v = std::fs::read(seg("receipts.seg")).unwrap();
    std::fs::write(seg("receipts.seg"), &v[..v.len() - 1]).unwrap();
    let e = open(&dir, Network::Apothem, 0).err().unwrap();
    assert!(e.to_string().contains("shorter"), "{e}");
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

/// Past the window the store model keeps no record and no segment bytes of a block (memory follows the
/// window, not the chain); its receipts are read back from the files, and a restart still opens at
/// the head.
#[test]
fn memory_follows_the_window() {
    use xone_node::services::core::RETENTION;
    use xone_sync::store::Store;
    let dir = scratch("window");
    let n = 3 * RETENTION as usize;
    let blocks = vectors(n);
    let core = open(&dir, Network::Apothem, 0).unwrap();
    for b in &blocks[1..] {
        let (h, body) = parts(b);
        core.import(h, body, xone_node::services::core::now())
            .unwrap();
    }
    let (old, new) = (blocks[10].header.hash(), blocks[n].header.hash());
    assert!(core.chain.with_store(|s| s.record(old)).is_none());
    assert!(core.chain.with_store(|s| s.record(new)).is_some());
    {
        let h = core.hist.read();
        let with_txs = h.blocks[1..n - RETENTION as usize]
            .iter()
            .find(|b| !b.body.transactions.is_empty())
            .expect("a block with a transaction below the window");
        assert!(with_txs.receipts.is_empty());
        let back = core
            .chain
            .with_store(|s| s.receipts(with_txs.header.number))
            .unwrap();
        assert_eq!(back.len(), with_txs.body.transactions.len());
    }
    drop(core);
    assert_eq!(
        open(&dir, Network::Apothem, 0).unwrap().head(),
        (n as u64, new)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
