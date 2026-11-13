#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! The snap download into the store's own tables (`--state.disk`, `SnapState`): the same download as
//! the in-memory `StateStore`, a kill at several points resumed from the rows and cursors the tables
//! hold, and a pivot that moves.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use alloy_primitives::{keccak256, Bytes, B256, U256};
use xone_db::mdbx::{Env, T};
use xone_snap::{
    client::{sync, Backend, Config, PeerError, StateStore, SyncError},
    server::{Limits, Server, Snapshot},
    testkit::{addr, flat_state, Loopback},
};
use xone_state::{trie::flat_root, FlatRead, FlatWrite, MemFlat};
use xone_sync::mdbx::snapstate::SnapState;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xone-snapstate-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn env(d: &std::path::Path) -> Arc<Env> {
    let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
    Arc::new(Env::open(d, &names).unwrap())
}

fn server(flat: &MemFlat, codes: &BTreeMap<B256, Bytes>) -> Server {
    Server::new(Snapshot::from_flat(flat, codes.clone()), Limits::default())
}

fn small() -> Config {
    Config {
        request_bytes: 32 * 1024,
        ..Config::default()
    }
}

fn changed(n: usize) -> (MemFlat, BTreeMap<B256, Bytes>) {
    let (mut flat, codes) = flat_state(n);
    for i in (0..2_000).step_by(13) {
        if let Some(mut a) = flat.account(addr(i)) {
            a.nonce += 1;
            flat.set_account(addr(i), Some(a));
        }
    }
    for i in (0..2_000).step_by(1000).skip(1) {
        flat.set_account(addr(i), None);
    }
    let slot = |s: usize| keccak256(format!("slot0-{s}"));
    flat.set_slot(addr(0), slot(5), U256::from(123_456u64));
    flat.set_slot(addr(0), slot(7), U256::ZERO);
    flat.set_slot(addr(0), slot(4000), U256::from(1u8));
    (flat, codes)
}

#[test]
fn the_table_backend_downloads_the_state_the_memory_store_does() {
    let (flat, codes) = flat_state(3_000);
    let want = flat_root(&flat);
    let mut mem = StateStore::new(want);
    sync(
        &mut mem,
        &mut Loopback::new(server(&flat, &codes)),
        &small(),
    )
    .unwrap();
    let d = dir("same");
    let mut disk = SnapState::open(env(&d), want).unwrap();
    sync(
        &mut disk,
        &mut Loopback::new(server(&flat, &codes)),
        &small(),
    )
    .unwrap();
    assert_eq!(disk.progress().verified, Some(want));
    assert_eq!(disk.root().unwrap(), want);
    assert_eq!(disk.accounts().unwrap(), mem.accounts);
    for (a, slots) in &mem.storage {
        assert_eq!(&disk.slots(a).unwrap(), slots);
    }
    assert_eq!(disk.codes().unwrap(), mem.codes);
    let (accounts, slots, nodes) = disk.counts().unwrap();
    assert_eq!(accounts, mem.accounts.len());
    assert_eq!(
        slots,
        mem.storage.values().map(BTreeMap::len).sum::<usize>()
    );
    assert_eq!(nodes, mem.codes.len());
    assert_eq!(disk.progress().storage_next, mem.progress.storage_next);
    let _ = std::fs::remove_dir_all(&d);
}

/// A kill at several requests: the tables are what the disk holds, a new `SnapState` over them goes on
/// without asking again for a finished range.
#[test]
fn a_killed_table_download_resumes_from_its_cursors() {
    let (flat, codes) = flat_state(3_000);
    let want = flat_root(&flat);
    let total = {
        let d = dir("fresh");
        let mut p = Loopback::new(server(&flat, &codes));
        sync(
            &mut SnapState::open(env(&d), want).unwrap(),
            &mut p,
            &small(),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&d);
        p.log.len()
    };
    for kill in [0usize, 1, 2, 5, 13, total / 2, total - 1] {
        let d = dir(&format!("kill{kill}"));
        let mut first = Loopback::new(server(&flat, &codes));
        first.budget = Some(kill);
        let before = {
            let mut s = SnapState::open(env(&d), want).unwrap();
            let r = sync(&mut s, &mut first, &small());
            assert!(
                matches!(r, Err(SyncError::Peer(PeerError::Disconnected))),
                "kill {kill}: {r:?}"
            );
            s.progress().clone()
        }; // the environment is dropped: a restart
        let mut s = SnapState::open(env(&d), want).unwrap();
        assert_eq!(
            s.progress().next_account,
            before.next_account,
            "kill {kill}"
        );
        assert_eq!(
            s.progress().storage_next,
            before.storage_next,
            "kill {kill}"
        );
        let mut second = Loopback::new(server(&flat, &codes));
        sync(&mut s, &mut second, &small()).unwrap_or_else(|e| panic!("kill {kill}: {e}"));
        assert_eq!(s.root().unwrap(), want, "kill {kill}");
        assert_eq!(s.progress().verified, Some(want));
        if let Some(n) = before.next_account {
            for m in &second.log {
                if let xone_snap::codec::Message::GetAccountRange(r) = m {
                    assert!(r.origin >= n, "kill {kill}: account range below the cursor");
                }
            }
        }
        assert!(
            first.log.len() + second.log.len() <= total + 1,
            "kill {kill}: {} + {} > {total} + 1",
            first.log.len(),
            second.log.len()
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The pivot moves while the state downloads (and the process dies once): what came from the old
/// root stays, the rest comes from the new one, the heal pass makes the result exactly the new state.
#[test]
fn a_moved_pivot_is_healed_in_the_tables() {
    let (flat_a, codes_a) = flat_state(2_000);
    let (flat_b, codes_b) = changed(2_300);
    let (root_a, root_b) = (flat_root(&flat_a), flat_root(&flat_b));
    assert_ne!(root_a, root_b);
    let want = Snapshot::from_flat(&flat_b, codes_b.clone());
    for kill in [1usize, 7, 40] {
        let d = dir(&format!("move{kill}"));
        {
            let mut s = SnapState::open(env(&d), root_a).unwrap();
            let mut a = Loopback::new(server(&flat_a, &codes_a));
            a.budget = Some(kill);
            let _ = sync(&mut s, &mut a, &small());
        }
        // a restart with another pivot: the cursors of the old one stay, the pivot is replaced
        let mut s = SnapState::open(env(&d), root_b).unwrap();
        assert!(
            s.progress().stale || s.counts().unwrap().0 == 0,
            "kill {kill}"
        );
        let mut b = Loopback::new(server(&flat_b, &codes_b));
        sync(&mut s, &mut b, &small()).unwrap_or_else(|e| panic!("kill {kill}: {e}"));
        assert_eq!(s.progress().verified, Some(root_b), "kill {kill}");
        assert_eq!(s.root().unwrap(), root_b);
        assert_eq!(s.accounts().unwrap().len(), want.account_count());
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Responses that answer fewer accounts than were asked (a byte budget of 2 KB): the accounts they
/// leave out are asked for again, and the download ends with every storage range done.
#[test]
fn short_storage_answers_leave_no_account_behind() {
    let (flat, codes) = flat_state(3_000);
    let want = flat_root(&flat);
    let tiny = Config {
        request_bytes: 2 * 1024,
        storage_accounts: 64,
        ..Config::default()
    };
    let d = dir("short");
    let mut s = SnapState::open(env(&d), want).unwrap();
    sync(&mut s, &mut Loopback::new(server(&flat, &codes)), &tiny).unwrap();
    assert_eq!(s.root().unwrap(), want);
    assert!(s.settled().unwrap());
    let _ = std::fs::remove_dir_all(&d);
}
