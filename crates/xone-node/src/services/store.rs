//! The node's store: [`MdbxStore`] over `<datadir>/xone/` (ADR-005): `chain.mdbx` (state, indexes,
//! changesets, the manifest), `static/{headers,bodies,receipts}/` (CRC-32C segment frames, 100,000
//! blocks per file), `journal/`, and `state/ckpt-<number>.json.gz`, the state checkpoints of the
//! replay harness that carry what the node's in-memory history needs beyond the store (V1 snapshots,
//! deployed code, sign transactions, V2 gap pools).
//!
//! A restart loads the newest checkpoint, reads the headers of the history's window below it and the
//! bodies of the blocks above it, unwinds the store to it and replays them through `Chain::insert`, so
//! that nothing stored is trusted without being verified and executed again. Below the window the
//! history reads headers, `TxLookup`, `ConsensusEpoch` and `ConsensusSnapshot` from the store
//! (`hist.rs`); bodies and receipts below the memory window are read when asked for ([`records`]).

use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use xone_consensus::verify::Body;
use xone_db::segments::{OsDir, BLOCKS_PER_FILE};
use xone_primitives::{Header, WideBalance};
use xone_state::{Account, FlatRead, FlatWrite, MemFlat};
use xone_sync::{
    mdbx::{Config, MdbxStore},
    store::Store,
};

/// The node's store: real MDBX, real files.
pub type NodeStore = MdbxStore<OsDir>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FileError(pub String);

fn io<T>(what: &Path, r: std::io::Result<T>) -> Result<T, FileError> {
    r.map_err(|e| FileError(format!("{}: {e}", what.display())))
}

/// Opens (or creates) the store of `<dir>`: takes the datadir lock and opens MDBX, with group commit
/// for `Some` (`xone_sync::mdbx::Config::group_commit`). Reconciliation is `Chain::open`'s or
/// `resume`'s.
pub fn open(
    dir: &Path,
    retention: u64,
    group_commit: Option<std::time::Duration>,
    disk_state: bool,
) -> Result<NodeStore, FileError> {
    if dir.join("head.json").exists() {
        return Err(FileError(format!(
            "{} holds the file store of an older build; it cannot be opened (remove the datadir and sync again)",
            dir.display()
        )));
    }
    let seg = io(dir, OsDir::open(dir))?;
    MdbxStore::open(
        &dir.join("chain.mdbx"),
        seg,
        Config {
            retention,
            per_file: BLOCKS_PER_FILE,
            group_commit,
            disk_state,
        },
    )
    .map_err(|e| FileError(format!("{}: {e}", dir.display())))
}

/// What a start-up reads from the store: the head, checked against its stored header, and the first
/// block held. Headers, bodies and receipts stay in the store.
pub struct Loaded {
    pub number: u64,
    pub hash: B256,
    /// The first block held: 1 for a node that followed from genesis, the first retained header of an
    /// imported snapshot otherwise.
    pub first: u64,
    /// Bodies and receipts below this block are not held ([`records`] gives empty ones).
    pub records_from: u64,
    /// The manifest of the snapshot the datadir was made from.
    pub anchor: Option<Bytes>,
}

/// Reads the committed head back. `None` for a store that holds nothing.
pub fn load(store: &NodeStore) -> Result<Option<Loaded>, FileError> {
    let Some(m) = store.manifest() else {
        return Ok(None);
    };
    let anchor = store.anchor();
    let (h_from, records_from) = (m.entries[0].first_block, m.entries[1].first_block);
    let first = if anchor.is_some() { h_from } else { 1 };
    let (number, hash) = store.head();
    let head = store
        .canonical_header(number)
        .map_err(|e| FileError(e.to_string()))?;
    if head.as_ref().map(Header::hash) != Some(hash) {
        return Err(FileError(
            "the last stored header is not the head of the manifest".into(),
        ));
    }
    Ok(Some(Loaded {
        number,
        hash,
        first,
        records_from,
        anchor,
    }))
}

/// Body and receipts of stored block `n`; empty ones below `records_from`.
pub fn records(store: &NodeStore, l: &Loaded, n: u64) -> Result<(Body, Vec<Bytes>), FileError> {
    if n < l.records_from {
        return Ok((Body::default(), vec![]));
    }
    let (_, body, receipts) = store
        .canonical_block(n)
        .map_err(|e| FileError(e.to_string()))?
        .ok_or_else(|| FileError(format!("block {n} is missing from the store")))?;
    Ok((body, receipts))
}

/// The state checkpoint of the replay harness (`conformance/harness/src/replay.rs`), plus the V2 gap
/// pools. `signs` are the sign transactions of each block as its own execution saw them.
#[derive(Serialize, Deserialize)]
pub struct Ckpt {
    pub number: u64,
    pub hash: B256,
    /// Rows keyed by address and slot: the form of checkpoints written before the hashed-key store,
    /// and of the replay harness. Read, no longer written by the node.
    pub accounts: Vec<(Address, u64, Bytes, B256)>,
    pub storage: Vec<(Address, B256, U256)>,
    /// Rows keyed by `keccak(address)` and `keccak(slot)` (ADR-005 amendment 2026-11-12): the node's
    /// checkpoints, which a snap-synced datadir can write without any preimage.
    #[serde(default)]
    pub haccounts: Vec<(B256, u64, Bytes, B256)>,
    #[serde(default)]
    pub hstorage: Vec<(B256, B256, U256)>,
    pub codes: BTreeMap<B256, Bytes>,
    pub signers: Vec<Address>,
    pub recents: BTreeMap<u64, Address>,
    pub signs: BTreeMap<u64, Vec<(Address, B256, bool)>>,
    #[serde(default)]
    pub gaps: Vec<(u64, Vec<Address>)>,
    /// The first block whose sign transactions `signs` holds (the history's window); 0 for all.
    #[serde(default)]
    pub signs_from: u64,
    /// Total difficulty at `number` (0 for a checkpoint of an imported snapshot).
    #[serde(default)]
    pub td: U256,
}

impl Ckpt {
    pub fn flat(&self) -> MemFlat {
        let mut f = MemFlat::default();
        for (a, nonce, bal, code_hash) in &self.accounts {
            f.set_account(
                *a,
                Some(Account {
                    nonce: *nonce,
                    balance: WideBalance::from_be_bytes(bal),
                    code_hash: *code_hash,
                    ..Default::default()
                }),
            );
        }
        for (a, k, v) in &self.storage {
            f.set_slot(*a, *k, *v);
        }
        for (h, nonce, bal, code_hash) in &self.haccounts {
            f.set_hashed_account(
                *h,
                Some(Account {
                    nonce: *nonce,
                    balance: WideBalance::from_be_bytes(bal),
                    code_hash: *code_hash,
                    ..Default::default()
                }),
            );
        }
        for (h, k, v) in &self.hstorage {
            f.set_hashed_slot(*h, *k, *v);
        }
        f
    }

    /// A checkpoint without state rows: the node's own, with `--state.disk` (the rows are in the store).
    pub fn without_state(number: u64, hash: B256) -> Self {
        Self::of_flat(number, hash, &MemFlat::default())
    }

    /// The tables of `flat`, with the parts the caller owns left empty.
    pub fn of_flat(number: u64, hash: B256, flat: &MemFlat) -> Self {
        let accounts: Vec<_> = flat.hashed_accounts().collect();
        Self {
            number,
            hash,
            accounts: vec![],
            storage: vec![],
            hstorage: accounts
                .iter()
                .flat_map(|(h, _)| flat.hashed_slots(*h).map(|(k, v)| (*h, k, v)))
                .collect(),
            haccounts: accounts
                .into_iter()
                .map(|(h, x)| {
                    (
                        h,
                        x.nonce,
                        Bytes::from(x.balance.to_be_bytes()),
                        x.code_hash,
                    )
                })
                .collect(),
            codes: BTreeMap::new(),
            signers: vec![],
            recents: BTreeMap::new(),
            signs: BTreeMap::new(),
            gaps: vec![],
            signs_from: 0,
            td: U256::ZERO,
        }
    }
}

fn ckpt_dir(dir: &Path) -> PathBuf {
    dir.join("state")
}

pub fn save_ckpt(dir: &Path, c: &Ckpt) -> Result<(), FileError> {
    let d = ckpt_dir(dir);
    io(&d, fs::create_dir_all(&d))?;
    let tmp = d.join("ckpt.tmp");
    let z = GzEncoder::new(io(&tmp, File::create(&tmp))?, Compression::fast());
    serde_json::to_writer(z, c).map_err(|e| FileError(e.to_string()))?;
    io(&tmp, io(&tmp, File::open(&tmp))?.sync_all())?;
    let to = d.join(format!("ckpt-{:08}.json.gz", c.number));
    io(&to, fs::rename(&tmp, &to))
}

/// Removes the state checkpoints below `number` (`Core::checkpoint` says which are kept).
pub fn drop_ckpts_below(dir: &Path, number: u64) {
    if let Ok(rd) = fs::read_dir(ckpt_dir(dir)) {
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix("ckpt-")
                .and_then(|r| r.strip_suffix(".json.gz"))
                .and_then(|r| r.parse::<u64>().ok());
            if n.is_some_and(|n| n < number) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// The newest checkpoint at or below `upto` that `ok` accepts (it is of this chain).
pub fn load_ckpt(dir: &Path, upto: u64, ok: impl Fn(&Ckpt) -> bool) -> Option<Ckpt> {
    let mut files: Vec<_> = fs::read_dir(ckpt_dir(dir))
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("ckpt-"))
        .map(|e| e.path())
        .collect();
    files.sort();
    files.iter().rev().find_map(|f| {
        let c: Ckpt = serde_json::from_reader(GzDecoder::new(File::open(f).ok()?)).ok()?;
        (c.number <= upto && ok(&c)).then_some(c)
    })
}
