//! The node's store: [`MdbxStore`] over `<datadir>/xone/` (ADR-005): `chain.mdbx` (state, indexes,
//! changesets, the manifest), `static/{headers,bodies,receipts}/` (CRC-32C segment frames, 100,000
//! blocks per file), `journal/`, and `state/ckpt-<number>.json.gz`, the state checkpoints of the
//! replay harness that carry what the node's in-memory history needs beyond the store (V1 snapshots,
//! deployed code, sign transactions, V2 gap pools).
//!
//! A restart reads the committed blocks back from the store, loads the newest checkpoint, unwinds the
//! store to it and replays the blocks above it through `Chain::insert`, so that nothing stored is
//! trusted without being verified and executed again.
//!
//! ponytail: the history (`hist.rs`) is still memory only and rebuilt this way. Persisting its rows in
//! the consensus tables of the store (`ConsensusEpoch`, `ConsensusSnapshot`, `Aux`) replaces the
//! checkpoint files and the replay; nothing above this module changes.

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

/// Opens (or creates) the store of `<dir>`: takes the datadir lock and opens MDBX. Reconciliation is
/// `Chain::open`'s or `resume`'s.
pub fn open(dir: &Path, retention: u64) -> Result<NodeStore, FileError> {
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
        },
    )
    .map_err(|e| FileError(format!("{}: {e}", dir.display())))
}

/// A block as the store holds it.
pub struct Stored3 {
    pub header: Header,
    pub body: Body,
    pub receipts: Vec<Bytes>,
}

/// What a start-up reads from the store: blocks `first..=number`, every frame CRC-checked.
pub struct Loaded {
    pub number: u64,
    pub hash: B256,
    /// The first block held: 1 for a node that followed from genesis, the first retained header of an
    /// imported snapshot otherwise.
    pub first: u64,
    /// Bodies and receipts below this block are not held (the blocks carry empty ones).
    pub records_from: u64,
    /// The manifest of the snapshot the datadir was made from.
    pub anchor: Option<Bytes>,
    pub blocks: Vec<Stored3>,
}

/// Reads the committed blocks back. `None` for a store that holds nothing.
pub fn load(store: &NodeStore) -> Result<Option<Loaded>, FileError> {
    let Some(m) = store.manifest() else {
        return Ok(None);
    };
    let anchor = store.anchor();
    let (h_from, records_from) = (m.entries[0].first_block, m.entries[1].first_block);
    let first = if anchor.is_some() { h_from } else { 1 };
    let (number, hash) = store.head();
    let mut blocks = Vec::new();
    for n in first..=number {
        let missing = || FileError(format!("block {n} is missing from the store"));
        let e = |e: xone_sync::store::StoreError| FileError(e.to_string());
        if n < records_from {
            let header = store.canonical_header(n).map_err(e)?.ok_or_else(missing)?;
            blocks.push(Stored3 {
                header,
                body: Body::default(),
                receipts: vec![],
            });
            continue;
        }
        let (header, body, receipts) = store.canonical_block(n).map_err(e)?.ok_or_else(missing)?;
        blocks.push(Stored3 {
            header,
            body,
            receipts,
        });
    }
    if blocks.last().map_or(0, |b| b.header.number) != number
        || blocks.last().map(|b| b.header.hash()).unwrap_or(hash) != hash
    {
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
        blocks,
    }))
}

/// The state checkpoint of the replay harness (`conformance/harness/src/replay.rs`), plus the V2 gap
/// pools. `signs` are the sign transactions of each block as its own execution saw them.
#[derive(Serialize, Deserialize)]
pub struct Ckpt {
    pub number: u64,
    pub hash: B256,
    pub accounts: Vec<(Address, u64, Bytes, B256)>,
    pub storage: Vec<(Address, B256, U256)>,
    pub codes: BTreeMap<B256, Bytes>,
    pub signers: Vec<Address>,
    pub recents: BTreeMap<u64, Address>,
    pub signs: BTreeMap<u64, Vec<(Address, B256, bool)>>,
    #[serde(default)]
    pub gaps: Vec<(u64, Vec<Address>)>,
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
        f
    }

    /// The tables of `flat`, with the parts the caller owns left empty.
    pub fn of_flat(number: u64, hash: B256, flat: &MemFlat) -> Self {
        let accounts: Vec<_> = flat.accounts().collect();
        Self {
            number,
            hash,
            storage: accounts
                .iter()
                .flat_map(|(a, _)| flat.slots(*a).map(|(k, v)| (*a, k, v)))
                .collect(),
            accounts: accounts
                .into_iter()
                .map(|(a, x)| {
                    (
                        a,
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

/// Minimal mode keeps the newest state checkpoint only: the older ones are state of blocks the node
/// has pruned.
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
