#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! `--state.disk` (issue #145, part 1): a node on the disk flat state imports the committed Apothem
//! vector blocks, restarts from a checkpoint that carries no state rows, replays and verifies the
//! blocks above it. A datadir of the memory state opens on the disk state too.

mod common;

use std::path::Path;

use common::{parts, scratch, vectors};
use xone_chainspec::Network;
use xone_node::services::core::{now, Core, CoreError};

const N: usize = 12;

fn open(dir: &Path, every: u64, disk: bool) -> Result<Core, CoreError> {
    let net = Network::Apothem;
    Core::open_full(
        net.spec(),
        &net.genesis().unwrap(),
        &dir.join("xone"),
        every,
        false,
        false,
        disk,
    )
}

#[test]
fn disk_state_follows_and_restarts() {
    let dir = scratch("diskstate");
    let blocks = vectors(N + 1);
    let want = blocks[N].header.hash();
    {
        let core = open(&dir, 5, true).unwrap();
        for b in &blocks[1..=N] {
            let (h, body) = parts(b);
            core.import(h, body, now()).unwrap();
        }
        assert_eq!(core.head(), (N as u64, want));
        assert_eq!(
            core.chain.with_store(|s| s.state_root()),
            blocks[N].header.state_root
        );
        // some reads went through the tables
        assert!(core.chain.with_store(|s| s.disk_stats()).is_some());
    }
    // the checkpoint of block 10 holds no state rows
    let ck = xone_node::services::store::load_ckpt(&dir.join("xone"), 10, |_| true).unwrap();
    assert_eq!(ck.number, 10);
    assert!(ck.haccounts.is_empty() && ck.hstorage.is_empty() && ck.accounts.is_empty());

    // restart on the disk state: replays 11 and 12 from the checkpoint of block 10
    let core = open(&dir, 5, true).unwrap();
    assert_eq!(core.head(), (N as u64, want));
    let (h, body) = parts(&blocks[N + 1]);
    core.import(h, body, now()).unwrap();
    assert_eq!(core.head().0, N as u64 + 1);
    drop(core);

    // the same datadir without the flag is refused with the reason (its checkpoints have no rows)
    let e = open(&dir, 5, false).err().unwrap();
    assert!(e.to_string().contains("--state.disk"), "{e}");
    // and on disk again
    let core = open(&dir, 5, true).unwrap();
    assert_eq!(core.head().0, N as u64 + 1);
    assert_eq!(
        core.chain.with_store(|s| s.state_root()),
        blocks[N + 1].header.state_root
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_memory_datadir_opens_on_the_disk_state() {
    let dir = scratch("diskstate-from-mem");
    let blocks = vectors(N);
    {
        let core = open(&dir, 5, false).unwrap();
        for b in &blocks[1..=N] {
            let (h, body) = parts(b);
            core.import(h, body, now()).unwrap();
        }
    }
    // the checkpoints carry rows; the disk state ignores them and replays 11 and 12 from the tables
    let core = open(&dir, 5, true).unwrap();
    assert_eq!(core.head(), (N as u64, blocks[N].header.hash()));
    assert_eq!(
        core.chain.with_store(|s| s.state_root()),
        blocks[N].header.state_root
    );
    let _ = std::fs::remove_dir_all(&dir);
}
