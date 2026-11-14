use std::{fs, path::Path, process::ExitCode};

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
        let tmp = temp_path(path);
        // `.gz`: a gzip archive, as geth writes it
        let gz = xone_sync::archive::is_gz_name(Path::new(path));
        let mut w = xone_sync::archive::create_block_file_as(Path::new(&tmp), gz)
            .map_err(|e| format!("{tmp}: {e}"))?;
        let n = match chain.with_store(|s| write(s, first, last, &mut w)) {
            Ok(n) => n,
            Err(e) => {
                drop(w);
                let _ = fs::remove_file(&tmp);
                return Err(e.to_string());
            }
        };
        if let Err(e) = w.finish() {
            let _ = fs::remove_file(&tmp);
            return Err(format!("{path}: {e}"));
        }
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("{path}: {e}")
        })?;
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

/// The sibling file the export is written to before it replaces `path`. An existing
/// file at `path` stays unchanged until the export has been completely written and synced.
fn temp_path(path: &str) -> String {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("export");
    let tmp = format!(".{name}.partial");
    match p.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => dir.join(tmp).to_string_lossy().into_owned(),
        None => tmp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_path_is_a_hidden_sibling() {
        assert_eq!(temp_path("chain.rlp"), ".chain.rlp.partial");
        assert_eq!(
            temp_path("/tmp/out/chain.rlp"),
            "/tmp/out/.chain.rlp.partial"
        );
    }
}
