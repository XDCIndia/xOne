use std::process::ExitCode;

use clap::ArgMatches;

use super::{fail, load, network, open, peak_rss_kib};

/// `xone import <file>`: the geth `importChain` shape (`cmd/geth/chaincmd.go:365`).
pub fn import(m: &ArgMatches) -> ExitCode {
    let run = || -> Result<String, String> {
        let chain = open(network(m)?)?;
        let path = m.get_one::<String>("file").map_or("", String::as_str);
        let (sum, secs) = load(&chain, path)?;
        let (n, hash) = chain.head();
        Ok(format!(
            "imported {} blocks in {secs:.1}s ({:.0} blocks/s), head {n} {hash}, peak RSS {} MiB",
            sum.executed,
            sum.executed as f64 / secs.max(1e-9),
            peak_rss_kib().map_or_else(|| "?".into(), |k| (k / 1024).to_string()),
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
