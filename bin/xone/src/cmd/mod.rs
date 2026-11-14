//! `xone import` and `xone export`: geth-compatible block files through the staged pipeline.
//!
//! ponytail: the datadir has no block database yet (the MDBX binding of ADR-005 is not written), so
//! both commands run on an in-memory chain started from the network's genesis. `import` verifies a
//! file end to end with the real executor and reports; `export` reads its range from the chain it
//! has just imported from `--input`. They move to the datadir with the store.

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

pub use export::export;
pub use import::import;
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
