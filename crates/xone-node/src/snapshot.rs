//! `xone snapshot export` and `xone snapshot import` on a node datadir (XB-18, issue #38). The
//! snapshot logic is `xone_sync::bootstrap`; this is the part that knows the files of the node:
//! the segment files and `head.json`, the state checkpoint with the codes, the V1 snapshot, the gap
//! pools and the sign transactions, and the anchor file (`services::store`).
//!
//! The datadir is `<datadir>/xone`, as for `xone start`. An export opens it as the node would,
//! replays what is above the newest checkpoint, and reads the snapshot from the opened chain. An
//! import writes the state checkpoint first and `head.json` last: a datadir exists when `head.json`
//! does.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use alloy_primitives::{Bytes, B256};
use xone_chainspec::{ChainSpec, Genesis, Network};
use xone_sync::{
    bootstrap::{
        export::{export, Exported, Params},
        import::{verify, Config, ImportError, Imported, Staging},
        manifest::{default_at, parse_pins, Side, SignRow},
        Context,
    },
    store::Store,
};

use crate::services::{
    core::{Core, CoreError, RETENTION},
    store::{self, load, save_ckpt, Ckpt},
};

/// The files of an interrupted import: a prefix of the snapshot file made of whole chunks.
pub struct FileStaging(pub PathBuf);

impl Staging for FileStaging {
    fn len(&self) -> u64 {
        fs::metadata(&self.0).map_or(0, |m| m.len())
    }
    fn reader(&self) -> std::io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(File::open(&self.0)?))
    }
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.0)?;
        f.write_all(bytes)?;
        f.sync_all()
    }
    fn truncate(&mut self, len: u64) -> std::io::Result<()> {
        if len == 0 {
            return match fs::remove_file(&self.0) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let f = OpenOptions::new().write(true).open(&self.0)?;
        f.set_len(len)?;
        f.sync_all()
    }
}

/// The network a datadir was made for, from its chain marker.
pub fn network_of(datadir: &Path) -> Result<Network, String> {
    let m = crate::datadir::marker(datadir).map_err(|e| e.to_string())?;
    Network::ALL
        .into_iter()
        .find(|n| n.spec().chain_id == m.chain_id)
        .ok_or_else(|| format!("chain id {} has no built-in network", m.chain_id))
}

fn open_core(spec: &ChainSpec, genesis: &Genesis, dir: &Path) -> Result<Core, String> {
    match Core::open(spec.clone(), genesis, dir, 0) {
        Err(CoreError::Anchored) => Core::open_with(spec.clone(), genesis, dir, 0, true),
        r => r,
    }
    .map_err(|e| e.to_string())
}

/// What a node's history says about the V1 snapshot, pools, codes and signs, for the pivot `at`.
fn side_of(core: &Core, at: u64) -> Result<Side, String> {
    let h = core.hist.read();
    let mut side = Side {
        codes: h.codes.clone(),
        ..Side::default()
    };
    if let Some(s) = h.snaps.get(&at) {
        side.signers = s.get_signers();
        side.recents = s.recents.clone();
    } else if core
        .spec
        .xdpos
        .v2
        .as_ref()
        .is_none_or(|v| at <= v.switch_block)
    {
        return Err(format!("no V1 signer snapshot of block {at} in memory"));
    }
    side.gaps = h
        .gaps
        .iter()
        .map(|(n, g)| (*n, g.candidates.clone()))
        .collect();
    for b in h
        .blocks
        .iter()
        .filter(|b| b.header.number <= at && !b.signs.is_empty())
    {
        side.signs.insert(
            b.header.number,
            b.signs
                .iter()
                .map(|s| SignRow {
                    sender: s.sender,
                    signed: s.signed,
                    ok: s.ok,
                })
                .collect(),
        );
    }
    Ok(side)
}

/// Writes an anchored snapshot of the datadir's own state to `out` (through `out.part`, renamed
/// when complete). `at` defaults to the latest block that has a proof: the latest committed epoch
/// switch on V2, the highest sealed block on V1.
pub fn export_datadir(
    spec: &ChainSpec,
    genesis: &Genesis,
    datadir: &Path,
    out: &Path,
    at: Option<u64>,
    now: u64,
    max_age: u64,
) -> Result<Exported, String> {
    let core = open_core(spec, genesis, &datadir.join("xone"))?;
    let (head, _) = core.chain.head();
    let at = match at {
        Some(a) => a,
        None => {
            let committed = core.chain.committed().map(|l| l.number);
            let reader = core.chain.reader();
            let header = |n: u64| {
                use xone_sync::chain::ChainRead;
                reader.header(reader.canonical_hash(n)?)
            };
            default_at(spec, &header, head, committed).map_err(|e| e.to_string())?
        }
    };
    let side = side_of(&core, at)?;
    let part = PathBuf::from(format!("{}.part", out.display()));
    let file = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut w = BufWriter::new(file);
    let done = core
        .chain
        .with_store(|s| {
            export(
                spec,
                s,
                &side,
                &Params {
                    at,
                    now,
                    max_age,
                    chunk_bytes: 1 << 20,
                },
                &mut w,
            )
        })
        .map_err(|e| e.to_string())?;
    let f = w.into_inner().map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(done)
}

/// Verifies `input` against `pins_file` and installs it as the datadir's whole content. Any
/// refusal leaves the datadir without `head.json`; an interruption leaves `snapshot.part` for the
/// next run. `max_age_cap` is the verifier's limit on the age of a manifest.
#[allow(clippy::too_many_arguments)]
pub fn import_datadir(
    spec: &ChainSpec,
    datadir: &Path,
    input: &Path,
    pins_file: Option<&Path>,
    now: u64,
    max_age_cap: u64,
) -> Result<Imported, ImportDatadirError> {
    let dir = datadir.join("xone");
    let pins = match pins_file {
        Some(p) => parse_pins(
            &fs::read_to_string(p)
                .map_err(|e| ImportDatadirError::Io(format!("{}: {e}", p.display())))?,
        )
        .map_err(|e| ImportDatadirError::Io(format!("{}: {e}", p.display())))?,
        None => vec![],
    };
    fs::create_dir_all(&dir)
        .map_err(|e| ImportDatadirError::Io(format!("{}: {e}", dir.display())))?;
    let mut store =
        store::open(&dir, RETENTION).map_err(|e| ImportDatadirError::Io(e.to_string()))?;
    if load(&store)
        .map_err(|e| ImportDatadirError::Io(e.to_string()))?
        .is_some()
    {
        return Err(ImportDatadirError::NotEmpty);
    }
    let cfg = Config {
        spec,
        ctx: Context {
            now,
            pins: &pins,
            max_age_cap,
        },
        retention: RETENTION,
    };
    let mut file = File::open(input)
        .map_err(|e| ImportDatadirError::Io(format!("{}: {e}", input.display())))?;
    let mut stage = FileStaging(dir.join("snapshot.part"));
    let v = verify(&cfg, &mut file, &mut stage)?;
    // the state checkpoint first, then the store in one commit
    let flat = v.install.state.clone();
    let mut ck = Ckpt {
        number: v.anchor.number,
        hash: v.anchor.hash,
        accounts: flat
            .accounts
            .iter()
            .map(|(a, x)| {
                (
                    *a,
                    x.nonce,
                    Bytes::from(x.balance.to_be_bytes()),
                    x.code_hash,
                )
            })
            .collect(),
        storage: flat.storage.clone(),
        codes: v.side.codes.clone(),
        signers: v.side.signers.clone(),
        recents: v.side.recents.clone(),
        signs: v
            .side
            .signs
            .iter()
            .map(|(n, r)| (*n, r.iter().map(|s| (s.sender, s.signed, s.ok)).collect()))
            .collect(),
        gaps: v.side.gaps.iter().map(|(n, c)| (*n, c.clone())).collect(),
    };
    ck.gaps.sort();
    save_ckpt(&dir, &ck).map_err(|e| ImportDatadirError::Io(e.to_string()))?;
    store
        .install_snapshot(v.install)
        .map_err(|e| ImportDatadirError::Import(ImportError::Store(e)))?;
    stage
        .truncate(0)
        .map_err(|e| ImportDatadirError::Io(e.to_string()))?;
    Ok(Imported {
        anchor: v.anchor,
        side: v.side,
        stats: v.stats,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ImportDatadirError {
    #[error("the datadir already holds a chain: a snapshot is imported into an empty datadir")]
    NotEmpty,
    #[error("{0}")]
    Io(String),
    #[error(transparent)]
    Import(#[from] ImportError),
}

/// A hash for the operator's log.
pub fn short(h: B256) -> String {
    let s = h.to_string();
    format!("{}..{}", &s[..10], &s[s.len() - 6..])
}
