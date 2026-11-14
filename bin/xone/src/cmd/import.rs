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

/// `xone import <file>` into the datadir: the blocks above the datadir's head, each through
/// `Core::import` (header rules, execution, durable commit), as the follower imports them.
pub fn import_datadir(m: &ArgMatches, cfg: &xone_node::Config) -> ExitCode {
    use xone_node::services::core::now;
    use xone_sync::chain::insert::Outcome;
    use xone_sync::stages::Source;
    let run = || -> Result<String, String> {
        let core = super::datadir(cfg)?;
        let path = m.get_one::<String>("file").map_or("", String::as_str);
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut src = xone_sync::import::FileSource::new(std::io::BufReader::new(f));
        let t0 = std::time::Instant::now();
        let mut done = 0u64;
        loop {
            let from = core.head().0 + 1;
            let headers = src.headers(from, 256).map_err(|e| e.to_string())?;
            if headers.is_empty() {
                break;
            }
            let bodies = src.bodies(&headers, 16 << 20).map_err(|e| e.to_string())?;
            if bodies.is_empty() {
                return Err(format!("the file has no body for block {from}"));
            }
            for (h, b) in headers.into_iter().zip(bodies) {
                let n = h.number;
                match core.import(h, b, now()) {
                    Ok(Outcome::Extended) => done += 1,
                    Ok(o) => return Err(format!("block {n}: not adopted: {o:?}")),
                    Err(e) => return Err(format!("block {n}: {e}")),
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let (n, hash) = core.head();
        Ok(format!(
            "imported {done} blocks in {secs:.1}s ({:.0} blocks/s) into {}, head {n} {hash}, peak RSS {} MiB",
            done as f64 / secs.max(1e-9),
            cfg.datadir.display(),
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
