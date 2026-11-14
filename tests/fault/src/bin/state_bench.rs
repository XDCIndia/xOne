//! `xone-state-bench <datadir> <accounts> <slots> <disk 0|1>`: commits a synthetic state through the
//! real store (MDBX and segment files on disk, retention 8, group commit), 5,000 accounts and
//! 20,000 slots per block, and prints the peak resident memory (and its anonymous and file-page parts) and the time. For the numbers of
//! `--state.disk` (issue #145). Test tool, never shipped.

use std::{path::PathBuf, process::ExitCode, time::Instant};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use xone_consensus::verify::Body;
use xone_db::segments::{OsDir, BLOCKS_PER_FILE};
use xone_primitives::{Header, WideBalance};
use xone_state::{Account, MemFlat, StateDiff};
use xone_sync::{
    mdbx::{Config, MdbxStore},
    store::{Block, Record, Store},
};

const PER_BLOCK_ACCOUNTS: u64 = 5_000;
const PER_BLOCK_SLOTS: u64 = 20_000;
const CONTRACTS: u64 = 1_000;

/// A `kB` field of `/proc/self/status` in MB.
fn status_mb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(field))
                .and_then(|l| l.split_whitespace().next().map(str::to_owned))
        })
        .and_then(|n| n.parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

fn addr(i: u64) -> Address {
    Address::from_slice(&keccak256(i.to_be_bytes())[..20])
}

fn run() -> Result<(), String> {
    let usage = "usage: xone-state-bench <datadir> <accounts> <slots> <disk 0|1>";
    let mut a = std::env::args().skip(1);
    let dir = PathBuf::from(a.next().ok_or(usage)?);
    let accounts: u64 = a.next().ok_or(usage)?.parse().map_err(|_| usage)?;
    let slots: u64 = a.next().ok_or(usage)?.parse().map_err(|_| usage)?;
    let disk = a.next().ok_or(usage)? == "1";
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let seg = OsDir::open(&dir).map_err(|e| e.to_string())?;
    let mut store = MdbxStore::open(
        &dir.join("chain.mdbx"),
        seg,
        Config {
            retention: 8,
            per_file: BLOCKS_PER_FILE,
            group_commit: Some(std::time::Duration::from_secs(3600)),
            disk_state: disk,
            geometry: Default::default(),
        },
    )
    .map_err(|e| e.to_string())?;
    // the genesis state is empty: its root is the root of an empty trie
    let genesis = Header {
        state_root: alloy_primitives::b256!(
            "56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
        ),
        ..Header::default()
    };
    let mut parent = genesis.hash();
    store
        .init_anchor(genesis, MemFlat::default())
        .map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let (mut na, mut ns, mut n) = (0u64, 0u64, 0u64);
    while na < accounts || ns < slots {
        n += 1;
        let mut diff = StateDiff::default();
        for _ in 0..PER_BLOCK_ACCOUNTS.min(accounts.saturating_sub(na)) {
            na += 1;
            diff.accounts.insert(
                addr(na),
                Some(Account {
                    nonce: 1,
                    balance: WideBalance::from(U256::from(na)),
                    ..Account::default()
                }),
            );
        }
        for _ in 0..PER_BLOCK_SLOTS.min(slots.saturating_sub(ns)) {
            ns += 1;
            diff.storage.insert(
                (
                    addr(1_000_000_000 + ns % CONTRACTS),
                    B256::from(U256::from(ns)),
                ),
                U256::from(ns),
            );
        }
        // the contracts are accounts too
        if n == 1 {
            for c in 0..CONTRACTS {
                diff.accounts
                    .insert(addr(1_000_000_000 + c), Some(Account::default()));
            }
        }
        let header = Header {
            number: n,
            parent_hash: parent,
            ..Header::default()
        };
        let rec = Record {
            block: Block::new(header, Body::default()),
            receipts: vec![],
            diff,
            aux: Bytes::new(),
        };
        parent = rec.block.hash();
        store.commit_block(&rec, None).map_err(|e| e.to_string())?;
        if n % 5 == 0 {
            // a steady commit frees the pages of the blocks before it (the file does not grow with them)
            store.steady().map_err(|e| e.to_string())?;
        }
    }
    store.steady().map_err(|e| e.to_string())?;
    if disk {
        let (rows, _) = store.trie_rows().map_err(|e| e.to_string())?;
        let (k, v) = rows.iter().fold((0usize, 0usize), |(k, v), (a, b)| {
            (k + a.len(), v + b.len())
        });
        println!(
            "trie rows {} (key bytes {} MB, value bytes {} MB)",
            rows.len(),
            k / 1_000_000,
            v / 1_000_000
        );
    }
    println!(
        "mode {} blocks {n} accounts {na} slots {ns} time {} s peak RSS {} MB (now: anonymous {} MB, file pages {} MB) cache {:?}",
        if disk { "disk" } else { "memory" },
        t0.elapsed().as_secs(),
        status_mb("VmHWM:"),
        status_mb("RssAnon:"),
        status_mb("RssFile:"),
        store.disk_stats()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xone-state-bench: {e}");
            ExitCode::FAILURE
        }
    }
}
