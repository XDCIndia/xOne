//! `xone import` and `xone export`: geth-compatible block files.
//!
//! By default both work on the node's datadir ([`datadir`]): the same chain marker, genesis and
//! exclusive lock as a node start, `import` through `Core::import` (the follower's validation and
//! durable commit) from the persisted head on, `export` from the stored canonical blocks. The
//! verification forms (`import --verify-only`, `export --input`) run the file through the staged
//! pipeline on an in-memory chain from the network's genesis and keep nothing.

mod export;
mod import;
mod snapshot;

use std::{fs::File, io::BufReader, process::ExitCode, time::Instant};

use clap::ArgMatches;
use xone_chainspec::Network;
use xone_node::services::core::now;
use xone_sync::{
    chain::{Chain, ChainRead},
    import::FileSource,
    stages::{self, Limits, Summary},
    testkit::MemStore,
    xdc::{genesis, XdcExecutor},
};

pub use export::{export, export_datadir};
pub use import::{import, import_datadir};
pub use snapshot::snapshot;

type Accept = fn(
    &dyn ChainRead,
    &xone_primitives::Header,
    &xone_primitives::Header,
    u64,
) -> Result<(), xone_consensus::verify::HeaderError>;
pub type Mem = Chain<MemStore, XdcExecutor, Accept>;

/// The header check of `execute_block` runs inside the executor, with the epoch context it holds.
fn accept(
    _: &dyn ChainRead,
    _: &xone_primitives::Header,
    _: &xone_primitives::Header,
    _: u64,
) -> Result<(), xone_consensus::verify::HeaderError> {
    Ok(())
}

fn network(m: &ArgMatches) -> Result<Network, String> {
    m.get_one::<String>("network")
        .map_or("apothem", String::as_str)
        .parse::<Network>()
        .map_err(|e| e.to_string())
}

/// A chain at the genesis block of `net`.
fn open(net: Network) -> Result<Mem, String> {
    let (g, flat, _) = genesis(net).map_err(|e| e.to_string())?;
    let store = MemStore::with_anchor(g, flat, 128, 100_000);
    let exec = XdcExecutor::new(net).map_err(|e| e.to_string())?;
    Ok(Chain::new(net.spec(), store, exec, accept as Accept))
}

/// The node's chain over its datadir, opened as a node start opens it (marker, genesis, lock).
/// Every block is committed to the device before the next (no group commit).
fn datadir(cfg: &xone_node::Config) -> Result<xone_node::services::core::Core, String> {
    let (marker, spec, genesis) =
        xone_node::datadir::prepare_chain(cfg).map_err(|e| e.to_string())?;
    let core = xone_node::services::core::Core::open_full(
        spec,
        &genesis,
        &cfg.datadir.join("xone"),
        xone_node::services::CHECKPOINT_EVERY,
        cfg.sync.minimal,
        false,
        cfg.sync.state_disk,
    )
    .map_err(|e| e.to_string())?;
    if core.genesis.to_string() != marker.genesis_hash {
        return Err("the genesis hash of the node differs from the chain marker".into());
    }
    Ok(core)
}

/// Imports the block file `path` into `chain`.
fn load(chain: &Mem, path: &str) -> Result<(Summary, f64), String> {
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let t0 = Instant::now();
    let sum = stages::sync(
        chain,
        &mut FileSource::new(BufReader::new(f)),
        &Limits::default(),
        now(),
    )
    .map_err(|e| e.to_string())?;
    Ok((sum, t0.elapsed().as_secs_f64()))
}

/// Peak resident set of this process in KiB (`VmHWM`), where `/proc` has it.
pub fn peak_rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("xone: {e}");
    ExitCode::from(1)
}
