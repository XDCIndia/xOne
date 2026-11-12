//! xOne command line (XB-19): geth-compatible flags, `init`, `dumpconfig`, a bounded
//! start and stop. `--version` and `version` print and exit before anything is read.

mod cmd;
mod flags;

use std::{ffi::OsString, process::ExitCode};

use clap::parser::ValueSource;
use xone_chainspec::Network;
use xone_node::{datadir, services, Config, Shutdown};

/// The one place the version stamp comes from.
pub fn stamp() -> &'static str {
    env!("XONE_STAMP")
}

fn genesis_hash(network: &str) -> Result<String, xone_chainspec::Error> {
    let network: Network = network.parse()?;
    Ok(network.genesis()?.hash(&network.spec())?.to_string())
}

fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("xone: {e}");
    ExitCode::from(1)
}

fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let (args, warnings) = flags::normalise(args);
    for w in warnings {
        eprintln!("xone: warning: {w}");
    }
    let m = match flags::command().try_get_matches_from(args) {
        Ok(m) => m,
        Err(e) => e.exit(), // usage errors exit 2, --help exits 0
    };
    let on = |k: &str| m.get_one::<String>(k).is_some_and(|v| v == "true");
    if on("version") || matches!(m.subcommand_name(), Some("version")) {
        println!("{}", stamp());
        return ExitCode::SUCCESS;
    }
    if let Some(("genesis-hash", s)) = m.subcommand() {
        let net = s.get_one::<String>("network").map_or("", String::as_str);
        return match genesis_hash(net) {
            Ok(h) => {
                println!("{h}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        };
    }
    match m.subcommand() {
        Some(("import", s)) => return cmd::import(s),
        Some(("export", s)) => return cmd::export(s),
        Some(("snapshot", s)) => return cmd::snapshot(s),
        _ => {}
    }
    let profiles = ["xdcmainnet", "apothem", "xdcdevnet", "net2040"];
    if profiles.iter().filter(|k| on(k)).count() + usize::from(m.contains_id("network")) > 1 {
        eprintln!(
            "xone: --xdcmainnet, --apothem, --xdcdevnet, --net2040 and --network are mutually exclusive"
        );
        return ExitCode::from(2);
    }
    let mut cfg = match m.get_one::<String>("config") {
        Some(p) => match Config::from_file(p.as_ref()) {
            Ok(c) => c,
            Err(e) => return fail(e),
        },
        None => Config::default(),
    };
    // flags win over the file
    for x in flags::FLAGS
        .iter()
        .filter(|x| !matches!(x.name, "config" | "version"))
    {
        if m.value_source(x.name) == Some(ValueSource::CommandLine) {
            if let Some(v) = m.get_one::<String>(x.name) {
                if let Err(e) = cfg.set(x.name, v) {
                    eprintln!("xone: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    }
    match m.subcommand() {
        Some(("dumpconfig", _)) => match cfg.to_toml() {
            Ok(t) => {
                print!("{t}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Some(("init", s)) => {
            let path = s.get_one::<String>("genesis").map_or("", String::as_str);
            let marker = std::fs::read_to_string(path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|json| datadir::init(&cfg.datadir, &json).map_err(|e| e.to_string()));
            match marker {
                Ok(m) => {
                    println!("chain id: {}\ngenesis hash: {}", m.chain_id, m.genesis_hash);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        _ => start(&cfg),
    }
}

/// Check the datadir marker, open the chain (a restart replays what is stored), then run net, chain,
/// pool and rpc until SIGTERM or SIGINT.
fn start(cfg: &Config) -> ExitCode {
    let (marker, spec, genesis) = match datadir::prepare_chain(cfg) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let shutdown = Shutdown::default();
    if let Err(e) = shutdown.install_signals() {
        return fail(e);
    }
    match services::snap_sync(cfg, &spec, &genesis, stamp(), &shutdown) {
        Ok(()) => {}
        Err(services::snapsync::SnapSyncError::Stopped) => {
            println!("stopped during snap sync; the datadir is still empty");
            return ExitCode::SUCCESS;
        }
        Err(e) => return fail(format!("snap sync: {e}")),
    }
    let mut node = match services::build(cfg, &marker, spec, &genesis, stamp()) {
        Ok(b) => b.node,
        Err(e) => return fail(e),
    };
    println!(
        "started chain {} genesis {}",
        marker.chain_id, marker.genesis_hash
    );
    match node.run(&shutdown) {
        Ok(()) => {
            println!("stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xone: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

fn main() -> ExitCode {
    run(std::env::args_os())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_shape() {
        let s = stamp();
        // xOne / v<semver>-<commit8>[-dirty]-<DDMMYYYY> / <os>-<arch> / rustc<version>
        let parts: Vec<_> = s.split('/').collect();
        assert_eq!(parts.len(), 4, "{s}");
        assert_eq!(parts[0], "xOne");
        assert!(parts[1].starts_with("v0.1.0-"), "{s}");
        let date = parts[1].rsplit('-').next().unwrap();
        assert!(
            date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()),
            "{s}"
        );
        assert_eq!(
            parts[2],
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        );
        assert!(parts[3].starts_with("rustc1."), "{s}");
    }

    #[test]
    fn genesis_hash_command_matches_geth() {
        assert_eq!(
            genesis_hash("mainnet").unwrap(),
            "0x4a9d748bd78a8d0385b67788c2435dcdb914f98a96250b68863a1f8b7642d6b1"
        );
        assert!(genesis_hash("nope").is_err());
    }

    /// Flags `join.sh` (ItWorksinMyLocal 24b915e) passes to geth (line 570) and xone (line 682).
    const JOIN_SH_GETH: &[&str] = &[
        "datadir",
        "networkid",
        "syncmode",
        "bootnodes",
        "port",
        "nat",
        "identity",
        "maxpeers",
        "http",
        "http.corsdomain",
        "http.addr",
        "http.port",
        "http.vhosts",
        "http.api",
        "ws",
        "ws.addr",
        "ws.origins",
        "ws.port",
        "ws.api",
        "authrpc.port",
        "ethstats",
    ];
    const JOIN_SH_XONE: &[&str] = &[
        "override.genesis",
        "networkid",
        "syncmode",
        "datadir",
        "port",
        "discovery.port",
        "nat",
        "bootnodes",
        "maxpeers",
        "http",
        "http.addr",
        "http.port",
        "http.vhosts",
        "http.api",
        "authrpc.port",
        "ethstats",
        "version",
    ];

    #[test]
    fn join_sh_flags_are_in_the_table() {
        for n in JOIN_SH_GETH.iter().chain(JOIN_SH_XONE) {
            assert!(flags::FLAGS.iter().any(|f| f.name == *n), "{n} missing");
        }
    }

    #[test]
    fn every_row_is_accepted_by_config() {
        for f in flags::FLAGS
            .iter()
            .filter(|f| !matches!(f.name, "config" | "version"))
        {
            let mut c = Config::default();
            let v = if f.value.is_some() { f.example } else { "true" };
            assert_eq!(
                c.set(f.name, v).map_err(|e| e.to_string()),
                Ok(()),
                "{}",
                f.name
            );
            for a in f.deprecated.iter().chain(f.aliases) {
                assert!(c.set(a, v).is_ok(), "{a}");
            }
        }
    }

    #[test]
    fn single_dash_is_normalised_and_deprecated_warns() {
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let (out, w) = flags::normalise(a(&[
            "xone",
            "-datadir",
            "d",
            "-http",
            "-http.port=9",
            "-txlookuplimit",
            "1",
            "-V",
            "-nope",
            "--",
            "-datadir",
        ]));
        assert_eq!(
            out,
            a(&[
                "xone",
                "--datadir",
                "d",
                "--http",
                "--http.port=9",
                "--txlookuplimit",
                "1",
                "-V",
                "-nope",
                "--",
                "-datadir"
            ])
        );
        assert_eq!(
            w,
            ["flag -txlookuplimit is deprecated, use --history.transactions"]
        );
    }

    #[test]
    fn flags_doc_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/cli/flags.md");
        let want = flags::render_doc();
        if std::env::var_os("UPDATE_DOCS").is_some() {
            std::fs::write(path, &want).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap_or_default(),
            want,
            "run UPDATE_DOCS=1 cargo test -p xone flags_doc"
        );
    }
}
