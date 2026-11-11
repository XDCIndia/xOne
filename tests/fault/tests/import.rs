#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)] // test code
//! The XB-14 import tests of `crates/xone-sync/tests/import.rs`, run on `MdbxStore` (real MDBX, the
//! segments on the in-memory disk): offline import of the committed Apothem vectors (blocks 0 to
//! 1,800) through the staged pipeline with the real executor, export and re-import byte for byte,
//! restart after a kill at each of ten points (under F1 and F2), and the pipeline bounds. Same
//! assertions as the MemStore originals; nothing expected was changed.

use alloy_primitives::B256;
use flate2::read::GzDecoder;
use std::{
    io::{Cursor, Read},
    path::Path,
};
use xone_chainspec::Network;
use xone_consensus::verify::Body;
use xone_consensus::verify::HeaderError;
use xone_db::fault::{MemDir, Pick};
use xone_fault::{anchored, cfg, copy_dir, Arm, Scratch};
use xone_primitives::Header;
use xone_sync::{
    chain::{resume::resume, Chain, ChainRead},
    export::encode_block,
    export::export,
    import::{decode_block, FileSource},
    mdbx::MdbxStore,
    stages::{
        self, bodies, execute, finish, headers, senders, Limits, Source, SourceError, SyncError,
    },
    store::{KillPoint, Stage, Store, StoreError},
    testkit::Fault,
    xdc::{genesis, XdcExecutor},
};

type Accept = fn(&dyn ChainRead, &Header, &Header, u64) -> Result<(), HeaderError>;
type MStore = MdbxStore<MemDir>;
type C = Chain<MStore, XdcExecutor, Accept>;

/// The header check of `execute_block` (`HeaderVerifier`) runs inside the executor, with the V1
/// epoch context it holds; the manager's own check hook has nothing more to add on Apothem V1.
fn accept(_: &dyn ChainRead, _: &Header, _: &Header, _: u64) -> Result<(), HeaderError> {
    Ok(())
}

const NOW: u64 = u64::MAX;
const NET: Network = Network::Apothem;

#[derive(serde::Deserialize)]
struct Line {
    n: u64,
    raw: alloy_primitives::Bytes,
    hash: Option<B256>,
}

/// The raw blocks 0..=`to` and the archive's own hashes.
fn raws(to: u64) -> (Vec<Vec<u8>>, Vec<(u64, B256)>) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/replay/apothem-0-1800.jsonl.gz");
    let mut text = String::new();
    GzDecoder::new(std::fs::File::open(p).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    let (mut blocks, mut hashes) = (Vec::new(), Vec::new());
    for l in text.lines().take(to as usize + 1) {
        let l: Line = serde_json::from_str(l).unwrap();
        blocks.push(l.raw.to_vec());
        hashes.extend(l.hash.map(|h| (l.n, h)));
    }
    (blocks, hashes)
}

/// The vector file as a geth block file: blocks 0..=`to` back to back.
fn vectors(to: u64) -> (Vec<u8>, Vec<(u64, B256)>) {
    let (blocks, hashes) = raws(to);
    (blocks.concat(), hashes)
}

/// A new datadir. The scratch directory is unlinked at once: MDBX holds its files open.
fn fresh_store_in(sc: &Scratch) -> MStore {
    let (g, flat, _) = genesis(NET).unwrap();
    anchored(sc, MemDir::new(), g, flat, cfg(128, 1_000))
}

fn fresh_store() -> MStore {
    fresh_store_in(&Scratch::new("import"))
}

fn chain_on(store: MStore, exec: XdcExecutor) -> C {
    Chain::new(NET.spec(), store, exec, accept as Accept)
}

fn fresh_chain() -> C {
    chain_on(fresh_store(), XdcExecutor::new(NET).unwrap())
}

/// What a restart does: reconcile, restore the executor context from the stored rows, open.
fn reopen(path: &std::path::Path, disk: MemDir) -> C {
    let mut store = MdbxStore::open(path, disk, cfg(128, 1_000)).unwrap();
    resume(&mut store).unwrap();
    let exec = XdcExecutor::restore(NET, &store).unwrap();
    chain_on(store, exec)
}

fn source(file: &[u8]) -> FileSource<Cursor<Vec<u8>>> {
    FileSource::new(Cursor::new(file.to_vec()))
}

#[test]
fn offline_import_of_apothem_0_to_1800_has_no_mismatch() {
    let (file, hashes) = vectors(1800);
    let chain = fresh_chain();
    let sum = stages::sync(&chain, &mut source(&file), &Limits::default(), NOW).unwrap();
    assert_eq!((sum.headers, sum.executed), (1800, 1800));
    let head = chain.head();
    assert_eq!(head.0, 1800);
    for (n, h) in &hashes {
        assert_eq!(chain.reader().canonical_hash(*n), Some(*h), "block {n}");
    }
    assert_eq!(
        hashes.len(),
        19,
        "every 100th block hash is the archive's own (blocks 0 to 1800)"
    );
    let header = chain.reader().header(head.1).unwrap();
    let store = chain.into_store();
    assert_eq!(
        store.flat_root(),
        header.state_root,
        "persisted state is the head's root"
    );
    for s in Stage::ALL {
        assert_eq!(store.checkpoint(s), 1800, "{s:?}");
    }
    assert!(store.staged_header(1800).is_none() && store.staged_body(1800).is_none());
}

#[test]
fn export_then_import_is_byte_equal() {
    let (file, _) = vectors(1800);
    let chain = fresh_chain();
    stages::sync(&chain, &mut source(&file), &Limits::default(), NOW).unwrap();
    let head = chain.head();
    let store = chain.into_store();
    let mut out = Vec::new();
    assert_eq!(export(&store, 0, 1800, &mut out).unwrap(), 1801);
    assert!(
        out == file,
        "export is not byte equal to the geth block file"
    );
    // and importing the exported file reaches the same head
    let again = fresh_chain();
    stages::sync(&again, &mut source(&out), &Limits::default(), NOW).unwrap();
    assert_eq!(again.head(), head);
    // a range, and the errors geth reports
    let mut part = Vec::new();
    export(&store, 100, 199, &mut part).unwrap();
    assert!(file.windows(part.len()).any(|w| w == &part[..]));
    assert!(export(&store, 5, 4, &mut Vec::new()).is_err());
    assert!(export(&store, 1700, 1801, &mut Vec::new()).is_err());
}

#[test]
fn import_skips_blocks_the_chain_holds_and_refuses_a_file_that_starts_too_high() {
    let (file, _) = vectors(300);
    let chain = fresh_chain();
    stages::sync(&chain, &mut source(&file), &Limits::default(), NOW).unwrap();
    // the same file again: nothing to do (geth `missingBlocks`)
    let sum = stages::sync(&chain, &mut source(&file), &Limits::default(), NOW).unwrap();
    assert_eq!((sum.headers, sum.executed), (0, 0));
    // a file that starts at block 200, on an empty chain
    let (blocks, _) = raws(300);
    let from200 = blocks[200..].concat();
    let e = stages::sync(
        &fresh_chain(),
        &mut source(&from200),
        &Limits::default(),
        NOW,
    )
    .unwrap_err();
    assert!(e.to_string().contains("starts at block 200"), "{e}");
    // and one that continues a chain that holds 0..=199
    let part = fresh_chain();
    stages::sync(
        &part,
        &mut source(&blocks[..200].concat()),
        &Limits::default(),
        NOW,
    )
    .unwrap();
    let sum = stages::sync(&part, &mut source(&from200), &Limits::default(), NOW).unwrap();
    assert_eq!((sum.headers, sum.executed, part.head().0), (101, 101, 300));
    // a truncated file is an error, not a hang
    let cut = &file[..file.len() - 5];
    assert!(stages::sync(&fresh_chain(), &mut source(cut), &Limits::default(), NOW).is_err());
}

fn limits() -> Limits {
    Limits {
        headers: 200,
        body_blocks: 50,
        body_bytes: 64 << 20,
        exec_blocks: 25,
        workers: 2,
    }
}

fn crashed(e: &SyncError) -> bool {
    matches!(
        e,
        SyncError::Store(StoreError::Crashed(_))
            | SyncError::Insert {
                error: xone_sync::chain::InsertError::Store(StoreError::Crashed(_)),
                ..
            }
    )
}

/// The ten points: the staged transaction before and after its commit (K14, K16) in each stage that
/// has one, and the execution commit path (K05 mid-execution, K13 segments synced, K16 commit
/// returned). `skip` picks a later batch of the stage.
const KILLS: [(Stage, KillPoint, usize); 10] = [
    (Stage::Headers, KillPoint::K14, 0),
    (Stage::Headers, KillPoint::K16, 0),
    (Stage::Bodies, KillPoint::K14, 1),
    (Stage::Bodies, KillPoint::K16, 2),
    (Stage::Senders, KillPoint::K16, 1),
    (Stage::Execution, KillPoint::K05, 30),
    (Stage::Execution, KillPoint::K13, 61),
    (Stage::Execution, KillPoint::K16, 120),
    (Stage::Finish, KillPoint::K14, 0),
    (Stage::Finish, KillPoint::K16, 0),
];

#[test]
fn a_kill_at_any_of_ten_points_resumes_at_the_stage_checkpoint() {
    const TO: u64 = 500; // through the V1 gap block 450, where the snapshot is reseeded
    let (file, hashes) = vectors(TO);
    // the clean run: its head is what every restarted run must reach
    let clean = fresh_chain();
    stages::sync(&clean, &mut source(&file), &limits(), NOW).unwrap();
    let want = clean.head();
    assert_eq!(want.0, TO);
    let want_root = clean.into_store().flat_root();

    for (power, (stage, point, skip)) in [false, true]
        .into_iter()
        .flat_map(|p| KILLS.into_iter().map(move |k| (p, k)))
    {
        let tag = format!("{stage:?} {point:?} skip {skip} power loss {power}");
        let sc = Scratch::new("import-kill");
        let mut chain = chain_on(fresh_store_in(&sc), XdcExecutor::new(NET).unwrap());
        let mut src = source(&file);
        let mut died = None;
        // all stages before the target run un-armed
        let mut rounds = 0;
        let mut armed = false;
        'outer: loop {
            rounds += 1;
            assert!(rounds < 200, "{tag}: never reached the kill point");
            for st in Stage::ALL {
                if st == stage && !armed {
                    chain.store_mut().arm(
                        point,
                        if power { Fault::Power } else { Fault::Kill },
                        skip,
                    );
                    armed = true;
                }
                let r: Result<u64, SyncError> = match st {
                    Stage::Headers => headers::run(&chain, &mut src, &limits()),
                    Stage::Bodies => bodies::run(&chain, &mut src, &limits()),
                    Stage::Senders => senders::run(&chain, &limits()),
                    Stage::Execution => execute::run(&chain, &limits(), NOW),
                    Stage::Finish => finish::run(&chain).map(u64::from),
                };
                match r {
                    Ok(_) => {}
                    Err(e) if crashed(&e) => {
                        died = Some(st);
                        break 'outer;
                    }
                    Err(e) => panic!("{tag}: {e}"),
                }
            }
            if chain.head().0 == TO && chain.with_store(|s| s.checkpoint(Stage::Finish)) == TO {
                break;
            }
        }
        died.unwrap_or_else(|| panic!("{tag}: the kill point was never reached"));
        let (pre_head, pre) = {
            let s = chain.store_mut();
            let pre: Vec<u64> = Stage::ALL.iter().map(|x| s.checkpoint(*x)).collect();
            (s.head().0, pre)
        };
        let dummy = fresh_store();
        let store = std::mem::replace(chain.store_mut(), dummy);
        // F1 for a kill -9, F2 for a power loss: the disk a restart finds
        let (path, dir) = store.into_parts();
        let rsc = Scratch::new("import-restart");
        copy_dir(&path, &rsc.path().join("chain.mdbx"));
        let reopened = reopen(
            &rsc.path().join("chain.mdbx"),
            dir.crash(if power {
                Pick::Nothing
            } else {
                Pick::Everything
            }),
        );
        // restart: no stage went back, and none is ahead of its predecessor
        let post: Vec<u64> = Stage::ALL
            .iter()
            .map(|x| reopened.with_store(|s| s.checkpoint(*x)))
            .collect();
        assert_eq!(
            post, pre,
            "{tag}: the checkpoints on disk are those the store reported"
        );
        assert_eq!(reopened.head().0, pre_head, "{tag}");
        let h0 = reopened.head().0;
        let staged = post[0].max(h0);
        // finish the import from a new source (a new process): only what is left runs
        let sum = stages::sync(&reopened, &mut Capped(source(&file), 0), &limits(), NOW).unwrap();
        assert_eq!(
            sum.headers,
            TO - staged,
            "{tag}: headers resume at the checkpoint"
        );
        assert_eq!(
            sum.executed,
            TO - h0,
            "{tag}: execution resumes at the head"
        );
        assert_eq!(reopened.head(), want, "{tag}");
        for (n, h) in &hashes {
            assert_eq!(
                reopened.reader().canonical_hash(*n),
                Some(*h),
                "{tag} block {n}"
            );
        }
        assert_eq!(reopened.into_store().flat_root(), want_root, "{tag}");
    }
}

/// A source that fails the test instead of looping when the pipeline asks too often.
struct Capped(FileSource<Cursor<Vec<u8>>>, u32);

impl Source for Capped {
    fn headers(&mut self, from: u64, max: usize) -> Result<Vec<Header>, SourceError> {
        self.1 += 1;
        assert!(
            self.1 < 100,
            "the headers stage asks again and again from {from}"
        );
        self.0.headers(from, max)
    }
    fn bodies(&mut self, hs: &[Header], max_bytes: usize) -> Result<Vec<Body>, SourceError> {
        self.0.bodies(hs, max_bytes)
    }
}

/// A source that records what the pipeline asks of it, and how far ahead of the head it asks.
struct Spy<'a> {
    inner: FileSource<Cursor<Vec<u8>>>,
    chain: &'a C,
    ahead: u64,
    header_calls: u64,
    returned: u64,
    max_batch: usize,
    body_calls: u64,
}

impl<'a> Spy<'a> {
    fn new(file: &[u8], chain: &'a C) -> Self {
        Self {
            inner: source(file),
            chain,
            ahead: 0,
            header_calls: 0,
            returned: 0,
            max_batch: 0,
            body_calls: 0,
        }
    }
}

impl Source for Spy<'_> {
    fn headers(&mut self, from: u64, max: usize) -> Result<Vec<Header>, SourceError> {
        self.header_calls += 1;
        self.ahead = self.ahead.max(from - 1 - self.chain.head().0);
        self.inner.headers(from, max)
    }
    fn bodies(&mut self, hs: &[Header], max_bytes: usize) -> Result<Vec<Body>, SourceError> {
        self.body_calls += 1;
        self.max_batch = self.max_batch.max(hs.len());
        let b = self.inner.bodies(hs, max_bytes)?;
        self.returned += b.len() as u64;
        Ok(b)
    }
}

/// xOneGo plan A3: the first snap path held every header in memory and failed at 1.9M of 85M.
/// Here the headers staged beyond the head never exceed the Q8 bound, however long the file.
#[test]
fn staged_headers_never_exceed_the_q8_window() {
    let (file, _) = vectors(400);
    let chain = fresh_chain();
    let lim = Limits {
        headers: 64,
        ..limits()
    };
    let mut spy = Spy::new(&file, &chain);
    let sum = stages::sync(&chain, &mut spy, &lim, NOW).unwrap();
    assert_eq!(sum.executed, 400);
    assert!(
        spy.ahead <= 64,
        "asked for headers {} above the head",
        spy.ahead
    );
    assert!(
        spy.header_calls >= 400 / 64,
        "the window moved: {}",
        spy.header_calls
    );
    assert!(
        chain.with_store(|s| s.staged_header(400).is_none()),
        "finish deleted the staged rows"
    );
}

/// xOneGo `3a0782af`, `bce8733b`: redundant body fetch rounds cut throughput. Every body is returned
/// once, in batches of at most Q9 blocks, and a byte budget shortens the batch instead of
/// re-requesting.
#[test]
fn every_body_is_fetched_once_in_bounded_batches() {
    let (file, _) = vectors(300);
    for (lim, min_calls) in [
        (limits(), 300 / 50),
        (
            Limits {
                body_bytes: 1,
                ..limits()
            },
            300 / 50,
        ), // at least one body per call
    ] {
        let chain = fresh_chain();
        let mut spy = Spy::new(&file, &chain);
        stages::sync(&chain, &mut spy, &lim, NOW).unwrap();
        assert_eq!(spy.returned, 300, "each body once");
        assert!(spy.max_batch <= lim.body_blocks);
        assert!(spy.body_calls >= min_calls);
        assert_eq!(chain.head().0, 300);
    }
}

/// Reorg: the stages and the chain unwind together, and the import repeats from there.
#[test]
fn unwinding_cuts_every_stage_and_the_import_repeats() {
    let (file, hashes) = vectors(300);
    let chain = fresh_chain();
    stages::sync(&chain, &mut source(&file), &limits(), NOW).unwrap();
    let head = chain.head();
    stages::unwind(&chain, 200).unwrap();
    assert_eq!(chain.head().0, 200);
    for st in Stage::ALL {
        assert_eq!(chain.with_store(|s| s.checkpoint(st)), 200, "{st:?}");
    }
    assert!(chain.with_store(|s| s.staged_header(250).is_none()));
    // the executor's context is above the new head: it is restored from the store, as at a restart
    let mut store = chain.into_store();
    resume(&mut store).unwrap();
    let exec = XdcExecutor::restore(NET, &store).unwrap();
    let chain = chain_on(store, exec);
    let sum = stages::sync(&chain, &mut source(&file), &limits(), NOW).unwrap();
    assert_eq!((sum.headers, sum.executed), (100, 100));
    assert_eq!(chain.head(), head);
    for (n, h) in hashes.iter().filter(|(n, _)| *n <= 300) {
        assert_eq!(chain.reader().canonical_hash(*n), Some(*h));
    }
}

#[test]
fn a_body_that_does_not_match_its_header_stops_before_execution() {
    let (blocks, _) = raws(40);
    // the first block of the vectors with a transaction
    let at = blocks
        .iter()
        .position(|b| !decode_block(b).unwrap().1.transactions.is_empty())
        .unwrap();
    let mut bad = blocks.clone();
    let (h, mut body) = decode_block(&bad[at]).unwrap();
    let last = body.transactions[0].len() - 1;
    let mut t = body.transactions[0].to_vec();
    t[last] ^= 1;
    body.transactions[0] = t.into();
    bad[at] = encode_block(&h, &body);
    let chain = fresh_chain();
    let e = stages::sync(&chain, &mut source(&bad.concat()), &limits(), NOW).unwrap_err();
    assert!(
        matches!(e, SyncError::Body { number, .. } if number == at as u64),
        "{e}"
    );
    assert_eq!(chain.head().0, 0, "nothing was executed");
}

#[test]
fn a_file_with_a_missing_block_does_not_link() {
    let (mut blocks, _) = raws(30);
    blocks.remove(11);
    let chain = fresh_chain();
    let e = stages::sync(&chain, &mut source(&blocks.concat()), &limits(), NOW).unwrap_err();
    assert!(matches!(e, SyncError::Link { number: 11 }), "{e}");
}
