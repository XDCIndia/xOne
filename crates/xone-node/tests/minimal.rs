#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Minimal mode (XB-18, issue #38) on the committed Apothem vectors, blocks 0..=1800 (V1: the
//! V2 window vectors need the 294 MB pivot state, which is not in the tree). A full node makes the
//! snapshot at block 100 and the state of block 100 is imported into an empty datadir. That node
//! then follows 101..=1800 from a fake peer only (the net and chain services, no other source): the
//! V1 epoch checkpoints 900 and 1800, and the reward of 1,800 (it reads 1,799 blocks of bodies, which
//! the snapshot carries). A second minimal node prunes below the derived floor as it follows, and is
//! restarted.

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::{B256, U256};
use common::{parts, scratch, vectors};
use tokio::runtime::Runtime;
use xone_chainspec::Network;
use xone_net::{
    eth::{Block as WireBlock, BlockBody, GetHeaders, Message, Origin},
    peers::{Config as PeersConfig, Event, Peers},
};
use xone_node::{
    datadir,
    services::{
        self,
        core::{now, Core},
    },
    snapshot::{export_datadir, import_datadir},
    Config,
};
use xone_state::trie::flat_root;
use xone_sync::bootstrap::{Class, Missing};

const AT: u64 = 100;
const DAY: u64 = 86_400;
const STAMP: &str = "xOneRS/v0.0.0-test/linux-x86_64/rustc1";

fn serve(
    rt: &Runtime,
    blocks: Arc<Vec<WireBlock>>,
    peers: Peers,
    mut rx: tokio::sync::mpsc::Receiver<Event>,
) {
    rt.spawn(async move {
        while let Some(ev) = rx.recv().await {
            let Event::Message { peer, msg } = ev else {
                continue;
            };
            let reply = match msg {
                Message::GetBlockHeaders {
                    id,
                    query:
                        GetHeaders {
                            origin,
                            amount,
                            skip,
                            reverse,
                        },
                } => {
                    let mut at = match origin {
                        Origin::Number(n) => Some(n),
                        Origin::Hash(h) => blocks
                            .iter()
                            .position(|b| b.header.hash() == h)
                            .map(|i| i as u64),
                    };
                    let mut headers = vec![];
                    while let Some(n) = at.filter(|_| (headers.len() as u64) < amount) {
                        let Some(b) = blocks.get(n as usize) else {
                            break;
                        };
                        headers.push(b.header.clone());
                        let step = skip + 1;
                        at = if reverse {
                            n.checked_sub(step)
                        } else {
                            Some(n + step)
                        };
                    }
                    Message::BlockHeaders { id, headers }
                }
                Message::GetBlockBodies { id, hashes } => Message::BlockBodies {
                    id,
                    bodies: hashes
                        .iter()
                        .filter_map(|h| blocks.iter().find(|b| b.header.hash() == *h))
                        .map(|b| BlockBody {
                            transactions: b.transactions.clone(),
                            uncles: b.uncles.clone(),
                            withdrawals: b.withdrawals.clone(),
                        })
                        .collect(),
                },
                _ => continue,
            };
            let _ = peers.send(&peer, reply);
        }
    });
}

/// A full node of the first 120 blocks, its snapshot at block 100, and the pins file.
fn snapshot(
    blocks: &[WireBlock],
    tag: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let net = Network::Apothem;
    let (spec, genesis) = (net.spec(), net.genesis().unwrap());
    let full = scratch(&format!("{tag}-full"));
    let core = Core::open(spec.clone(), &genesis, &full.join("xone"), 100).unwrap();
    for b in &blocks[1..=120] {
        let (h, body) = parts(b);
        core.import(h, body, now()).unwrap();
    }
    drop(core);
    let out = full.join("snapshot.xsnap");
    let t = Instant::now();
    let d = export_datadir(&spec, &genesis, &full, &out, Some(AT), now(), DAY).unwrap();
    println!(
        "export: block {} {} accounts {} slots {} codes {} headers {} bodies, {} bytes, {:.2}s",
        d.at,
        d.accounts,
        d.slots,
        d.codes,
        d.headers,
        d.bodies,
        d.bytes,
        t.elapsed().as_secs_f64()
    );
    let pins = full.join("pins.txt");
    std::fs::write(
        &pins,
        format!(
            "# the checkpoint\n{AT} {}\n",
            blocks[AT as usize].header.hash()
        ),
    )
    .unwrap();
    (full, out, pins)
}

fn import(blocks: &[WireBlock], tag: &str) -> std::path::PathBuf {
    let (_full, out, pins) = snapshot(blocks, tag);
    let min = scratch(&format!("{tag}-min"));
    let t = Instant::now();
    let done = import_datadir(
        &Network::Apothem.spec(),
        &min,
        &out,
        Some(&pins),
        now(),
        7 * DAY,
    )
    .unwrap();
    println!(
        "import: {} accounts {} slots {} chunks, {:.2}s",
        done.stats.accounts,
        done.stats.slots,
        done.stats.chunks,
        t.elapsed().as_secs_f64()
    );
    assert_eq!(
        done.anchor.state_root,
        blocks[AT as usize].header.state_root
    );
    let _ = std::fs::remove_dir_all(&_full);
    min
}

#[test]
#[ignore = "follows 1,700 blocks through the services: ~3 min release, >10 min debug; run with --ignored"]
fn a_minimal_node_follows_two_checkpoints_and_a_reward_from_a_fake_peer_only() {
    let blocks = Arc::new(vectors(1800));
    let min = import(&blocks, "peer");

    // refused: a minimal node needs --minimal, and a full node never starts as one
    let net = Network::Apothem;
    let (spec, genesis) = (net.spec(), net.genesis().unwrap());
    assert!(Core::open(spec.clone(), &genesis, &min.join("xone"), 0).is_err());
    let empty = scratch("peer-empty");
    assert!(Core::open_with(spec, &genesis, &empty.join("xone"), 0, true, false).is_err());

    let mut cfg = Config::default();
    cfg.set("apothem", "true").unwrap();
    cfg.set("minimal", "true").unwrap();
    cfg.datadir = min.clone();
    cfg.p2p.port = 0;
    cfg.p2p.no_discover = true;
    cfg.ipc.disabled = true;
    let (marker, net, genesis) = datadir::prepare_chain(&cfg).unwrap();
    let mut built = services::build(&cfg, &marker, net, &genesis, STAMP).unwrap();
    assert_eq!(built.core.head(), (AT, blocks[AT as usize].header.hash()));
    built.node.start().unwrap();

    let rt = Runtime::new().unwrap();
    let mut pc = PeersConfig::new("fake", 51, built.core.genesis);
    pc.head = blocks[1800].header.hash();
    pc.td = U256::from(10_000_000u64);
    let (fake, rx) = Peers::new(pc, secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap());
    serve(&rt, blocks.clone(), fake.clone(), rx);
    let (a, b) = tokio::io::duplex(1 << 20);
    let (ours, our_id) = (built.net.peers.clone(), built.net.id);
    let (r1, r2) =
        rt.block_on(async { tokio::join!(ours.attach(a, None), fake.attach(b, Some(our_id))) });
    r1.unwrap();
    r2.unwrap();

    let t = Instant::now();
    while built.core.head().0 < 1800 {
        assert!(
            t.elapsed() < Duration::from_secs(600),
            "head {:?}",
            built.core.head()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "followed 101..=1800 from the fake peer in {:.1}s",
        t.elapsed().as_secs_f64()
    );
    assert_eq!(built.core.head(), (1800, blocks[1800].header.hash()));
    let state = built.core.chain.with_store(|s| flat_root(s.head_flat()));
    assert_eq!(state, blocks[1800].header.state_root);
    built.node.stop().unwrap();
    let _ = std::fs::remove_dir_all(&min);
}

#[test]
fn a_minimal_node_prunes_to_its_floor_restarts_and_answers_for_the_rest_with_the_typed_error() {
    let blocks = vectors(1800);
    let min = import(&blocks, "prune");
    let net = Network::Apothem;
    let (spec, genesis) = (net.spec(), net.genesis().unwrap());
    let dir = min.join("xone");
    let mut core = Core::open_with(spec.clone(), &genesis, &dir, 100, true, false).unwrap();
    core.prune_slack = 0;
    let held0 = core.held().unwrap();
    assert_eq!((held0.headers_from, held0.records_from), (0, 0));
    for b in &blocks[101..=1800] {
        let (h, body) = parts(b);
        core.import(h, body, now()).unwrap();
    }
    assert_eq!(core.head(), (1800, blocks[1800].header.hash()));
    // the floor of head 1800 is 1 for headers and 2 for bodies and receipts (windows 1,800 and 1,799)
    let held = core.held().unwrap();
    assert_eq!((held.headers_from, held.records_from), (1, 2));
    assert!(held.is_within(Class::Header, 1).is_ok());
    assert_eq!(
        held.is_within(Class::Header, 0),
        Err(Missing::UnsupportedHistory {
            class: Class::Header,
            block: 0,
            floor: 1,
            pivot: 1800
        })
    );
    assert!(matches!(
        held.is_within(Class::Body, 1),
        Err(Missing::UnsupportedHistory {
            class: Class::Body,
            ..
        })
    ));
    {
        let h = core.hist.read();
        assert_eq!(h.base, 1);
        assert!(h.at(0).is_none() && h.at(1).is_some());
        assert!(h.at(1).unwrap().body.transactions.is_empty());
    }
    let want = (1800, blocks[1800].header.hash());
    // the import writes its checkpoints in the background; the close waits for the last one
    drop(core);
    // only the newest state checkpoint is kept
    let ckpts: Vec<_> = std::fs::read_dir(dir.join("state"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(ckpts.len(), 1, "{ckpts:?}");

    // restart: the files are the pruned generation, the checkpoint is block 1800
    let core = Core::open_with(spec, &genesis, &dir, 100, true, false).unwrap();
    assert_eq!(core.head(), want);
    assert_eq!(core.held().unwrap(), held);
    let root: B256 = core.chain.with_store(|s| flat_root(s.head_flat()));
    assert_eq!(root, blocks[1800].header.state_root);
    let _ = std::fs::remove_dir_all(&min);
}
