use std::{fs::File, io::BufWriter, process::ExitCode};

use clap::ArgMatches;
use xone_sync::export::export as write;

use super::{fail, load, network, open};

/// `xone export <file> [from] [to] --input <blockfile>`: the geth `exportChain` shape
/// (`cmd/geth/chaincmd.go:445`). Without `from` and `to` the whole chain is written.
pub fn export(m: &ArgMatches) -> ExitCode {
    let run = || -> Result<String, String> {
        let chain = open(network(m)?)?;
        let input = m.get_one::<String>("input").map_or("", String::as_str);
        load(&chain, input)?;
        let head = chain.head().0;
        let num = |k: &str, d: u64| {
            m.get_one::<String>(k)
                .map_or(Ok(d), |v| v.parse::<u64>().map_err(|e| format!("{k}: {e}")))
        };
        let (first, last) = (num("from", 0)?, num("to", head)?);
        if last > head {
            return Err(format!("block number {last} larger than head block {head}"));
        }
        let path = m.get_one::<String>("file").map_or("", String::as_str);
        let mut w = BufWriter::new(File::create(path).map_err(|e| format!("{path}: {e}"))?);
        let n = chain
            .with_store(|s| write(s, first, last, &mut w))
            .map_err(|e| e.to_string())?;
        Ok(format!("exported {n} blocks, {first}..={last}, to {path}"))
    };
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

/// `xone export <file> [from] [to]` from the datadir: its canonical blocks as stored, without
/// re-execution. A block the datadir does not hold (below a minimal node's floor) is an error.
pub fn export_datadir(m: &ArgMatches, cfg: &xone_node::Config) -> ExitCode {
    let run = || -> Result<String, String> {
        let core = super::datadir(cfg)?;
        let head = core.head().0;
        let num = |k: &str, d: u64| {
            m.get_one::<String>(k)
                .map_or(Ok(d), |v| v.parse::<u64>().map_err(|e| format!("{k}: {e}")))
        };
        let (first, last) = (num("from", 0)?, num("to", head)?);
        if first > last {
            return Err(format!("export range {first}..={last} is empty"));
        }
        if last > head {
            return Err(format!("block number {last} larger than head block {head}"));
        }
        let path = m.get_one::<String>("file").map_or("", String::as_str);
        let mut w = BufWriter::new(File::create(path).map_err(|e| format!("{path}: {e}"))?);
        let mut parent = None;
        for n in first..=last {
            let (header, hash) = {
                let h = core.hist.read();
                let hash = h
                    .hash_at(n)
                    .ok_or_else(|| format!("block {n} is not held"))?;
                let header = h
                    .header_at(n)
                    .map(std::borrow::Cow::into_owned)
                    .ok_or_else(|| format!("header of block {n} is not held"))?;
                (header, hash)
            };
            if parent.is_some_and(|p| p != header.parent_hash) {
                return Err(format!("block {n} does not link to block {}", n - 1));
            }
            let body = core
                .body(n, hash)
                .ok_or_else(|| format!("body of block {n} is not held"))?;
            parent = Some(hash);
            std::io::Write::write_all(&mut w, &xone_sync::export::encode_block(&header, &body))
                .map_err(|e| format!("{path}: {e}"))?;
        }
        std::io::Write::flush(&mut w).map_err(|e| format!("{path}: {e}"))?;
        Ok(format!(
            "exported {} blocks, {first}..={last}, from {} to {path}",
            last - first + 1,
            cfg.datadir.display()
        ))
    };
    match run() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}
