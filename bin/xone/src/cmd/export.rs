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
