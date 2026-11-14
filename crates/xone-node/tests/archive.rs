#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! `--gcmode archive` (issue #152): net2040 blocks 0..=899 imported with a 64-block state window.
//! The state of blocks far below the window is served from the history tables: its root, recomputed
//! from the rows, is the header's state root; the RPC reads and `eth_call` at an old block answer what
//! they answered while that block was the head; an unwind and the reimport restore the history rows,
//! and so does a restart (which replays from a checkpoint); the gcmode of a datadir is fixed.

mod common;

use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool, sync::Arc};

use alloy_primitives::{address, keccak256, Address, Bytes, B256, U256};
use common::{parts, scratch, vectors_of};
use xone_chainspec::Network;
use xone_node::services::{
    core::{now, Core, CoreError, Opts},
    rpc::RpcState,
};
use xone_rpc::api::{CallRequest, StateApi};
use xone_sync::{bootstrap::retention::Missing, chain::ChainRead};

const HEAD: usize = 899;
/// The state window of the test: blocks below `head - W` are served by the history only.
const W: u64 = 64;
const VALIDATOR: Address = address!("0000000000000000000000000000000000000088");

fn open(dir: &Path, archive: bool) -> Result<Core, CoreError> {
    let net = Network::Net2040;
    Core::open_opts(
        net.spec(),
        &net.genesis().unwrap(),
        &dir.join("xone"),
        50,
        Opts {
            archive,
            retention: W,
            ..Opts::default()
        },
    )
}

/// What the RPC answers at a block: balance and nonce of each address, a 0x88 slot, `getCandidates()`.
#[derive(Debug, PartialEq)]
struct Answer {
    accounts: Vec<(U256, u64)>,
    slot: U256,
    candidates: Bytes,
}

fn answer(rpc: &RpcState, hash: B256, addrs: &[Address]) -> Result<Answer, Missing> {
    let mut accounts = vec![];
    let mut slot = U256::ZERO;
    rpc.state(hash, &mut |s| {
        accounts = addrs
            .iter()
            .map(|a| {
                s.account(*a)
                    .map_or((U256::ZERO, 0), |x| (x.balance.to_u256().unwrap(), x.nonce))
            })
            .collect();
        slot = s.storage(VALIDATOR, B256::with_last_byte(1));
    })?;
    let req = CallRequest {
        from: None,
        to: Some(VALIDATOR),
        gas: Some(50_000_000),
        gas_price: None,
        max_fee: None,
        max_priority_fee: None,
        value: None,
        data: Bytes::copy_from_slice(&keccak256("getCandidates()")[..4]),
    };
    let out = rpc.call(hash, &req, &AtomicBool::new(false)).unwrap();
    assert_eq!(out.error, None);
    assert!(out.output.len() >= 64, "an ABI address list");
    Ok(Answer {
        accounts,
        slot,
        candidates: out.output,
    })
}

#[test]
fn archive_serves_the_state_of_every_block() {
    let dir = scratch("archive");
    let blocks = vectors_of("net2040-0-899.jsonl.gz", HEAD);
    let net = Network::Net2040;
    let mut addrs: Vec<Address> = net.genesis().unwrap().alloc.keys().copied().collect();
    addrs.extend(blocks.iter().map(|b| b.header.coinbase));
    addrs.sort();
    addrs.dedup();
    let sample = [0u64, 1, 16, 100, 449, 450, 700, 800];

    let core = Arc::new(open(&dir, true).unwrap());
    let rpc = RpcState::new(core.clone());
    // the answers while each sampled block is the head (the state window serves them)
    let mut at_head = BTreeMap::new();
    at_head.insert(0, answer(&rpc, blocks[0].header.hash(), &addrs).unwrap());
    for b in &blocks[1..=HEAD] {
        let (h, body) = parts(b);
        let n = h.number;
        core.import(h, body, now()).unwrap();
        if sample.contains(&n) {
            at_head.insert(n, answer(&rpc, b.header.hash(), &addrs).unwrap());
        }
    }
    assert_eq!(core.head(), (HEAD as u64, blocks[HEAD].header.hash()));
    let rows = core.history_rows().unwrap();
    assert!(rows.0 > 0, "{rows:?}");

    let check = |core: &Core, rpc: &RpcState| {
        for &n in &sample {
            let b = &blocks[usize::try_from(n).unwrap()];
            // below the window: the store's state window does not hold it, the history does
            assert!(core
                .chain
                .reader()
                .with_state(b.header.hash(), &mut |_| {})
                .is_err());
            assert_eq!(
                core.history_root(n).unwrap(),
                b.header.state_root,
                "the state root at {n}"
            );
            assert_eq!(
                answer(rpc, b.header.hash(), &addrs).unwrap(),
                at_head[&n],
                "the RPC at {n}"
            );
        }
    };
    check(&core, &rpc);

    // unwind 30 blocks: their history rows go with them; the reimport writes them again
    let back = HEAD as u64 - 30;
    core.unwind_to(back).unwrap();
    let fewer = core.history_rows().unwrap();
    assert!(fewer.0 < rows.0, "{fewer:?} {rows:?}");
    assert_eq!(
        core.history_root(back).unwrap(),
        blocks[HEAD - 30].header.state_root
    );
    for b in &blocks[HEAD - 29..=HEAD] {
        let (h, body) = parts(b);
        core.import(h, body, now()).unwrap();
    }
    assert_eq!(core.history_rows().unwrap(), rows);
    check(&core, &rpc);
    drop(rpc);
    drop(Arc::into_inner(core).unwrap());

    // a restart replays from the checkpoint of block 850 (an unwind and a reimport in the store)
    let core = Arc::new(open(&dir, true).unwrap());
    assert_eq!(core.head().0, HEAD as u64);
    assert_eq!(core.history_rows().unwrap(), rows);
    check(&core, &RpcState::new(core.clone()));
    drop(Arc::into_inner(core).unwrap());

    // the gcmode is the datadir's
    let e = open(&dir, false).err().unwrap();
    assert!(
        matches!(
            e,
            CoreError::GcMode {
                stored: "archive",
                asked: "full"
            }
        ),
        "{e}"
    );
    assert!(e.to_string().contains("--gcmode archive"), "{e}");
    let full = scratch("archive-full");
    {
        let core = Arc::new(open(&full, false).unwrap());
        let (h, body) = parts(&blocks[1]);
        core.import(h, body, now()).unwrap();
        assert_eq!(core.history_rows().unwrap(), (0, 0));
        // a full node refuses a block below its window with the typed error, not an empty answer
        for b in &blocks[2..=HEAD.min(80)] {
            let (h, body) = parts(b);
            core.import(h, body, now()).unwrap();
        }
        let r = answer(
            &RpcState::new(core.clone()),
            blocks[1].header.hash(),
            &addrs,
        );
        assert!(
            matches!(r, Err(Missing::UnsupportedHistory { block: 1, .. })),
            "{r:?}"
        );
    }
    let e = open(&full, true).err().unwrap();
    assert!(
        matches!(
            e,
            CoreError::GcMode {
                stored: "full",
                asked: "archive"
            }
        ),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&full);
}
