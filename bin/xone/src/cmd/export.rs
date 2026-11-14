use std::{
    fs::File,
    io::{BufWriter, Write},
    process::ExitCode,
};

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
        let n = atomic(path, |w| {
            chain
                .with_store(|s| write(s, first, last, w))
                .map_err(|e| e.to_string())
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

/// Writes through `<path>.tmp`, flushes and syncs it, then renames over `path`: a failed or
/// truncated export never replaces an existing file, and success means the bytes are on disk.
fn atomic<T>(
    path: &str,
    f: impl FnOnce(&mut BufWriter<File>) -> Result<T, String>,
) -> Result<T, String> {
    let tmp = format!("{path}.tmp");
    let io = |e: std::io::Error| format!("{path}: {e}");
    let mut w = BufWriter::new(File::create(&tmp).map_err(io)?);
    let done = f(&mut w).and_then(|n| {
        w.flush().map_err(io)?;
        w.get_ref().sync_all().map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)?;
        Ok(n)
    });
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_export_keeps_the_old_file_and_a_good_one_replaces_it() {
        let dir = std::env::temp_dir().join(format!("xone-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("out.rlp");
        let p = p.to_str().unwrap();
        std::fs::write(p, b"old").unwrap();
        let r = atomic(p, |w| {
            w.write_all(b"half").map_err(|e| e.to_string())?;
            Err::<(), _>("boom".into())
        });
        assert!(r.is_err());
        assert_eq!(std::fs::read(p).unwrap(), b"old");
        assert!(!std::path::Path::new(&format!("{p}.tmp")).exists());
        atomic(p, |w| w.write_all(b"new").map_err(|e| e.to_string())).unwrap();
        assert_eq!(std::fs::read(p).unwrap(), b"new");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
