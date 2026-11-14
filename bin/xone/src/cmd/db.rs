//! `xone db stats` (#176, the first of the `db` commands): per-table MDBX page counts of
//! `<datadir>/xone/chain.mdbx` and the file sizes of `static/{headers,bodies,receipts}`,
//! `journal/` and `state/`. The environment is opened read-only, so it runs beside a node.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::ArgMatches;
use xone_db::mdbx::{Env, TableStat, T};

use super::fail;

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024 * 10 && i + 1 < U.len() {
        v /= 1024;
        i += 1;
    }
    format!("{v} {}", U.get(i).unwrap_or(&"B"))
}

/// (files, bytes) under `dir`, recursively; a missing directory is (0, 0).
fn du(dir: &Path) -> (u64, u64) {
    let Ok(rd) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let (mut files, mut bytes) = (0u64, 0u64);
    for e in rd.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                let (f, b) = du(&e.path());
                files += f;
                bytes = bytes.saturating_add(b);
            }
            Ok(t) if t.is_file() => {
                files += 1;
                bytes = bytes.saturating_add(e.metadata().map_or(0, |m| m.len()));
            }
            _ => {}
        }
    }
    (files, bytes)
}

/// The report for the store directory `xone` (`<datadir>/xone`).
pub fn stats_report(store: &Path) -> Result<String, String> {
    let path = store.join("chain.mdbx");
    if !path.exists() {
        return Err(format!("{}: no chain database", path.display()));
    }
    let env = Env::open_read_only(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rd = env.read().map_err(|e| e.to_string())?;
    let mut o = String::new();
    let _ = writeln!(o, "chain.mdbx: {}", path.display());
    let _ = writeln!(
        o,
        "{:<20} {:>14} {:>5} {:>10} {:>12} {:>10} {:>12}",
        "Table", "Entries", "Depth", "Branch", "Leaf", "Overflow", "Size"
    );
    let mut total = TableStat::default();
    for t in T::ALL {
        let s = rd
            .stat_in(t.name())
            .map_err(|e| format!("{}: {e}", t.name()))?;
        let _ = writeln!(
            o,
            "{:<20} {:>14} {:>5} {:>10} {:>12} {:>10} {:>12}",
            t.name(),
            s.entries,
            s.depth,
            s.branch_pages,
            s.leaf_pages,
            s.overflow_pages,
            human(s.bytes())
        );
        total.entries = total.entries.saturating_add(s.entries);
        total.branch_pages = total.branch_pages.saturating_add(s.branch_pages);
        total.leaf_pages = total.leaf_pages.saturating_add(s.leaf_pages);
        total.overflow_pages = total.overflow_pages.saturating_add(s.overflow_pages);
        total.page_size = s.page_size;
    }
    let _ = writeln!(
        o,
        "{:<20} {:>14} {:>5} {:>10} {:>12} {:>10} {:>12}",
        "Total",
        total.entries,
        "",
        total.branch_pages,
        total.leaf_pages,
        total.overflow_pages,
        human(total.bytes())
    );
    drop(rd);
    let (map, used) = env.size().map_err(|e| e.to_string())?;
    let _ = writeln!(
        o,
        "page size {} B, used {} of map {} (freelist and meta pages included)\n",
        total.page_size,
        human(used),
        human(map)
    );
    let _ = writeln!(o, "{:<20} {:>8} {:>12}", "Directory", "Files", "Size");
    for d in [
        "static/headers",
        "static/bodies",
        "static/receipts",
        "journal",
        "state",
    ] {
        let (f, b) = du(&store.join(d));
        let _ = writeln!(o, "{d:<20} {f:>8} {:>12}", human(b));
    }
    Ok(o)
}

pub fn db(m: &ArgMatches) -> ExitCode {
    let Some(("stats", s)) = m.subcommand() else {
        return fail("db: a subcommand is required");
    };
    let datadir = s
        .get_one::<String>("datadir")
        .map_or_else(|| xone_node::Config::default().datadir, PathBuf::from);
    match stats_report(&datadir.join("xone")) {
        Ok(r) => {
            print!("{r}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_of_a_small_store() {
        let dir = std::env::temp_dir().join(format!("xone-dbstats-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let names: Vec<_> = T::ALL.iter().map(|t| t.name()).collect();
        {
            let env = Env::open(&dir.join("chain.mdbx"), &names).expect("open");
            let w = env.write().expect("write");
            for i in 0u32..10 {
                w.put(T::CanonicalHeaders, &i.to_be_bytes(), &[1u8; 32])
                    .expect("put");
            }
            w.commit().expect("commit");
        }
        fs::create_dir_all(dir.join("static/headers")).expect("dir");
        fs::write(dir.join("static/headers/00000.seg"), [0u8; 1000]).expect("seg");
        let r = stats_report(&dir).expect("report");
        let line = |name: &str| {
            r.lines()
                .find(|l| l.split_whitespace().next() == Some(name))
                .map(|l| l.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_else(Vec::new)
        };
        assert_eq!(
            line("CanonicalHeaders").get(1).map(String::as_str),
            Some("10"),
            "{r}"
        );
        assert_eq!(line("Total").get(1).map(String::as_str), Some("10"), "{r}");
        assert_eq!(
            line("static/headers").get(1).map(String::as_str),
            Some("1"),
            "{r}"
        );
        assert_eq!(
            line("static/headers").get(2).map(String::as_str),
            Some("1000"),
            "{r}"
        );
        assert!(stats_report(&dir.join("none")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
