//! `xone-db-stress <datadir> <blocks>`: imports the committed Apothem vectors, blocks 1 to `blocks`,
//! into the node's real datadir (MDBX and segment files on disk) through `Core`, and prints one
//! line per committed block. The kill harness (`tests/process_kill.rs`) sends it SIGKILL at random
//! points and starts it again on the same datadir. Test tool, never shipped.

use std::{
    io::{Read, Write},
    path::PathBuf,
    process::ExitCode,
};

use alloy_rlp::Decodable;
use flate2::read::GzDecoder;
use xone_chainspec::Network;
use xone_consensus::verify::Body;
use xone_net::eth::Block as WireBlock;
use xone_node::services::{
    core::{now, Core},
    from_wire,
};

const CHECKPOINT_EVERY: u64 = 50;

fn vectors(count: usize) -> Result<Vec<WireBlock>, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/replay/apothem-0-1800.jsonl.gz");
    let mut text = String::new();
    GzDecoder::new(std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    text.lines()
        .take(count + 1)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).map_err(|e| e.to_string())?;
            let raw = alloy_primitives::hex::decode(v["raw"].as_str().unwrap_or_default())
                .map_err(|e| e.to_string())?;
            WireBlock::decode(&mut &raw[..]).map_err(|e| e.to_string())
        })
        .collect()
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .ok_or("usage: xone-db-stress <datadir> <blocks>")?,
    );
    let count: usize = args
        .next()
        .ok_or("usage: xone-db-stress <datadir> <blocks>")?
        .parse()
        .map_err(|e| format!("{e}"))?;
    let blocks = vectors(count)?;
    let net = Network::Apothem;
    let core = Core::open(
        net.spec(),
        &net.genesis().map_err(|e| e.to_string())?,
        &dir.join("xone"),
        CHECKPOINT_EVERY,
    )
    .map_err(|e| format!("open: {e}"))?;
    let out = std::io::stdout();
    let say = |s: String| {
        let mut o = out.lock();
        let _ = writeln!(o, "{s}");
        let _ = o.flush();
    };
    say(format!("open {}", core.head().0));
    let start = usize::try_from(core.head().0).map_err(|e| e.to_string())? + 1;
    for b in blocks.iter().skip(start) {
        let body = Body {
            transactions: b.transactions.iter().map(from_wire).collect(),
            uncles: b.uncles.clone(),
            withdrawals: None,
        };
        core.import(b.header.clone(), body, now())
            .map_err(|e| format!("block {}: {e}", b.header.number))?;
        say(format!("head {}", b.header.number));
    }
    let (n, h) = core.head();
    say(format!("done {n} {h}"));
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xone-db-stress: {e}");
            ExitCode::FAILURE
        }
    }
}
