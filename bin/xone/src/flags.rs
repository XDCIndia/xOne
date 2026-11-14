//! The flag table. One list drives the clap command, the single-dash normaliser and the
//! generated `docs/cli/flags.md`, so they cannot disagree.

use std::ffi::OsString;

use clap::{Arg, ArgAction, Command};

// default, example and source feed the generated doc and the tests only.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Flag {
    pub name: &'static str,
    /// Accepted spellings besides `name`.
    pub aliases: &'static [&'static str],
    /// Accepted too, but a warning names `name` as the replacement.
    pub deprecated: &'static [&'static str],
    /// Value placeholder; None for a boolean flag.
    pub value: Option<&'static str>,
    pub default: &'static str,
    /// A value `Config::set` must accept (test only).
    pub example: &'static str,
    pub meaning: &'static str,
    /// File:line of the semantic, `;` then who passes it in join.sh at 24b915e.
    pub source: &'static str,
}

const fn f(
    name: &'static str,
    value: Option<&'static str>,
    default: &'static str,
    example: &'static str,
    meaning: &'static str,
    source: &'static str,
) -> Flag {
    Flag {
        name,
        aliases: &[],
        deprecated: &[],
        value,
        default,
        example,
        meaning,
        source,
    }
}

const B: Option<&str> = None;
const N: Option<&str> = Some("<n>");
const S: Option<&str> = Some("<string>");
const P: Option<&str> = Some("<path>");
const L: Option<&str> = Some("<a,b,..>");

pub const FLAGS: &[Flag] = &[
    f(
        "datadir",
        P,
        "xone-data",
        "/d",
        "Data directory for the databases and the chain marker.",
        "flags.go:93 ; geth,xone",
    ),
    f(
        "networkid",
        N,
        "chain id of the profile or genesis",
        "51",
        "Network id. Must equal the chain id of the genesis in use.",
        "flags.go:141 ; geth,xone",
    ),
    f(
        "syncmode",
        Some("full|snap|fast"),
        "full",
        "snap",
        "Blockchain sync mode. full executes from genesis. snap, on an empty datadir, downloads the state at a pivot over snap/1 with every range proven, from any snap/1 peer (geth-XDC included; the store is keyed by hash, no preimages are needed), installs it as an anchored datadir and executes from pivot+1 (spec/snap/pivot.md); on a datadir that holds a chain it continues that chain. fast is an alias of snap, as in geth-XDC.",
        "flags.go:299 ; geth,xone",
    ),
    f(
        "snap.pins",
        P,
        "unset",
        "pins.txt",
        "Pins file for --syncmode snap (`<number> <0x hash>` per line): the pivot's epoch-switch header (or the pivot) must be in it. Unset: two peers must serve the same switch header. Recommended beyond test networks.",
        "not in geth-XDC at the pin; XB-17",
    ),
    f(
        "snap.gap-rpc",
        P,
        "unset",
        "http://127.0.0.1:8545",
        "For --syncmode snap from geth-XDC peers: any archive RPC that serves eth_getProof. The 0x88 state of the gap blocks below the pivot is read from it (geth serves snap/1 only for recent state); every account and slot proof is verified against the header's state root, so the RPC is not trusted. Unset and no xOne peer: the start fails with a typed error naming both.",
        "xone; spec/snap/pivot.md",
    ),
    f(
        "state.disk",
        B,
        "false",
        "true",
        "Read the head state and its Merkle tries from the MDBX tables (behind bounded caches, trie nodes by position) instead of holding them in memory; checkpoints and the snap install carry no state rows. A memory-state datadir starts with it; one written with it must be started with it. A node on disk state serves no snap/1 or snapshot export (docs/ops/disk-state.md).",
        "xone; issue #145",
    ),
    f(
        "gcmode",
        Some("full"),
        "full",
        "full",
        "State garbage collection mode. Only full is implemented: archive is refused with a typed error (state is pruned to the state window).",
        "flags.go:338",
    ),
    f(
        "syncfromblock",
        N,
        "unset",
        "100",
        "First block to import; unset means from genesis. xone-defined.",
        "not in geth-XDC at the pin; task list",
    ),
    f(
        "minimal",
        B,
        "false",
        "true",
        "Start from the anchor of an imported snapshot (`xone snapshot import`), keep only the retention window, never sync from genesis or read an archive.",
        "not in geth-XDC at the pin; issue #38",
    ),
    f(
        "cache",
        N,
        "4096",
        "1024",
        "Megabytes of memory for internal caches.",
        "flags.go:556",
    ),
    Flag {
        aliases: &[],
        deprecated: &["txlookuplimit"],
        ..f(
            "history.transactions",
            N,
            "2350000",
            "0",
            "Recent blocks whose transactions stay indexed; 0 is the whole chain.",
            "flags.go:379",
        )
    },
    f(
        "port",
        N,
        "30303",
        "30304",
        "P2P listening port (TCP).",
        "flags.go:969 ; geth,xone",
    ),
    f(
        "discovery.port",
        N,
        "same as port",
        "30305",
        "P2P discovery port (UDP).",
        "flags.go:1026 ; xone",
    ),
    f(
        "maxpeers",
        N,
        "50",
        "25",
        "Maximum peers; 0 disables networking.",
        "flags.go:957 ; geth,xone",
    ),
    f(
        "nodiscover",
        B,
        "false",
        "true",
        "Disable peer discovery.",
        "flags.go:997",
    ),
    f(
        "nat",
        S,
        "unset",
        "extip:1.2.3.4",
        "NAT port mapping: any, none, upnp, pmp, extip:<IP>.",
        "flags.go:991 ; geth,xone",
    ),
    f(
        "bootnodes",
        L,
        "built-in for the profile",
        "enode://a@1.2.3.4:1",
        "Comma-separated enode URLs for discovery bootstrap.",
        "flags.go:975 ; geth,xone",
    ),
    f(
        "eth.requiredblocks",
        L,
        "built-in for the profile (Apothem: 83665617)",
        "83665617=0x2432ce6433608954ef6eaf2feba8f9a2fe90f5a913401eb07d5fa2f1bb499c3f",
        "Comma-separated `<number>=<0x hash>` entries. A peer that serves another hash at one of these heights is disconnected (reason 0x03) within 15 seconds of the handshake, and a header chain through another hash is refused before import or pivot choice. A peer that has not reached the height is kept. An entry replaces the built-in one at the same height. Apothem has one built-in entry, the first canonical block after the September 2026 reset; stale v2.8.2 peers still serve the abandoned branch. dumpconfig prints the merged list.",
        "flags.go:2265 (apothemResetBlockNum) ; geth,xone",
    ),
    f(
        "identity",
        S,
        "unset",
        "n1",
        "Node name shown to peers and ethstats.",
        "flags.go:199 ; geth",
    ),
    f(
        "http",
        B,
        "false",
        "true",
        "Enable the HTTP-RPC server.",
        "flags.go:829 ; geth,xone",
    ),
    f(
        "http.addr",
        S,
        "localhost",
        "0.0.0.0",
        "HTTP-RPC listen address.",
        "flags.go:834 ; geth,xone",
    ),
    f(
        "http.port",
        N,
        "8545",
        "8546",
        "HTTP-RPC listen port.",
        "flags.go:840 ; geth,xone",
    ),
    f(
        "http.api",
        L,
        "eth,net,web3",
        "eth,XDPoS",
        "APIs offered over HTTP-RPC.",
        "flags.go:858 ; geth,xone",
    ),
    f(
        "http.corsdomain",
        L,
        "none",
        "*",
        "Allowed CORS origins for HTTP-RPC.",
        "flags.go:846 ; geth",
    ),
    f(
        "http.vhosts",
        L,
        "localhost",
        "*",
        "Allowed virtual hosts for HTTP-RPC.",
        "flags.go:852 ; geth,xone",
    ),
    f(
        "ws",
        B,
        "false",
        "true",
        "Enable the WebSocket-RPC server.",
        "flags.go:887 ; geth",
    ),
    f(
        "ws.addr",
        S,
        "localhost",
        "0.0.0.0",
        "WebSocket-RPC listen address.",
        "flags.go:892 ; geth",
    ),
    f(
        "ws.port",
        N,
        "8546",
        "8547",
        "WebSocket-RPC listen port.",
        "flags.go:898 ; geth",
    ),
    f(
        "ws.api",
        L,
        "eth,net,web3",
        "eth",
        "APIs offered over WebSocket-RPC.",
        "flags.go:904 ; geth",
    ),
    f(
        "ws.origins",
        L,
        "none",
        "*",
        "Allowed origins for WebSocket-RPC.",
        "flags.go:910 ; geth",
    ),
    f(
        "rpc.jwtsecret",
        P,
        "unset",
        "/d/rpc-jwt.hex",
        "JWT secret file (32 bytes hex): HTTP and WebSocket RPC then require an HS256 bearer token. IPC is not affected.",
        "reth --rpc.jwtsecret; geth node/jwt_handler.go",
    ),
    f(
        "authrpc.addr",
        S,
        "localhost",
        "127.0.0.1",
        "Authenticated RPC listen address.",
        "flags.go:761",
    ),
    f(
        "authrpc.port",
        N,
        "8551",
        "8552",
        "Authenticated RPC listen port.",
        "flags.go:767 ; geth,xone",
    ),
    f(
        "authrpc.vhosts",
        L,
        "localhost",
        "*",
        "Allowed virtual hosts for authenticated RPC.",
        "flags.go:773",
    ),
    f(
        "authrpc.jwtsecret",
        P,
        "unset",
        "/d/jwt.hex",
        "Path of the JWT secret file for authenticated RPC.",
        "flags.go:779",
    ),
    f(
        "ipcdisable",
        B,
        "false",
        "true",
        "Disable the IPC server.",
        "flags.go:819",
    ),
    f(
        "ipcpath",
        P,
        "<datadir>/xone.ipc",
        "/d/x.ipc",
        "IPC socket path.",
        "flags.go:824",
    ),
    f(
        "mine",
        B,
        "false",
        "true",
        "Enable block production (deprecated spelling kept for XDC tooling).",
        "flags_legacy.go:125",
    ),
    f(
        "miner.etherbase",
        S,
        "unset",
        "0xaa",
        "Address that receives block rewards.",
        "flags_legacy.go:119",
    ),
    f(
        "unlock",
        L,
        "none",
        "0xaa",
        "Accounts to unlock, comma-separated.",
        "flags_legacy.go:144",
    ),
    f(
        "password",
        P,
        "unset",
        "/d/pw",
        "File with the account password; the password is never a flag.",
        "flags.go:672",
    ),
    f(
        "keystore",
        P,
        "unset",
        "/d/ks.json",
        "Keystore file of the validator key. Block production (--mine) needs it with --password, and is refused on chain id 50 in this build.",
        "flags.go:124 ; xone",
    ),
    f(
        "miner.quietwindow",
        N,
        "the network's",
        "300",
        "ADR-010 signer quiet window in seconds (`ChainSpec::quiet_window`: 120 on mainnet, Apothem and net2040, 20 on the devnets). Only a larger value is accepted.",
        "xone",
    ),
    f(
        "ethstats",
        S,
        "unset",
        "n:s@h:443",
        "Stats server as name:secret@host:port.",
        "flags.go:786 ; geth,xone",
    ),
    f(
        "verbosity",
        N,
        "3",
        "4",
        "Log level: 0 silent, 1 error, 2 warn, 3 info, 4 debug, 5 detail.",
        "internal/debug/flags.go:42",
    ),
    f(
        "log.format",
        Some("text|json"),
        "text",
        "json",
        "Log line format. json writes one object per line (t, lvl, target, msg, then the fields); the byte budgets and redaction are the same as text.",
        "internal/debug/flags.go:59",
    ),
    Flag {
        aliases: &[],
        deprecated: &["vmodule"],
        ..f(
            "log.vmodule",
            S,
            "unset",
            "xone_net=4,xone_sync::snap=5",
            "Per-target verbosity as target=level pairs; a target is a crate or module path (xone_net, xone_sync::snap), longest match wins. geth matches file patterns instead.",
            "internal/debug/flags.go:47",
        )
    },
    f(
        "debug.logslowblock",
        Some("<duration>"),
        "-1ns",
        "1s",
        "Log one JSON line with per-phase timings (verify, senders, execution, commit, history, total) for each imported block that took at least this long; 0 logs every block, a negative value is off.",
        "flags.go:795",
    ),
    f(
        "override.genesis",
        P,
        "unset",
        "/g.json",
        "Genesis file; the datadir is initialised from it on first start.",
        "flags.go:294 ; xone",
    ),
    f(
        "chainspec",
        P,
        "unset",
        "/forks.json",
        "Fork table of a private chain (one network, the format of generated/forks.json); refused for chain ids 50 and 51.",
        "not in geth-XDC; xone-defined",
    ),
    f(
        "xdcmainnet",
        B,
        "false",
        "true",
        "XDC mainnet profile (chain 50).",
        "flags.go:166",
    ),
    f(
        "apothem",
        B,
        "false",
        "true",
        "XDC Apothem profile (chain 51).",
        "flags.go:171",
    ),
    f(
        "xdcdevnet",
        B,
        "false",
        "true",
        "XDC devnet profile (chain 5551).",
        "flags.go:176",
    ),
    f(
        "net2040",
        B,
        "false",
        "true",
        "XDC net2040 end-to-end devnet profile (chain 2040).",
        "chainspec genesis/net2040.json ; xone",
    ),
    f(
        "network",
        S,
        "unset",
        "net2040",
        "Built-in network by name: mainnet, apothem, devnet or net2040; same as the profile flags.",
        "xone",
    ),
    f(
        "config",
        P,
        "unset",
        "",
        "TOML config file; flags override it. Not a config key.",
        "cmd/geth/config.go:65",
    ),
    f(
        "metrics",
        B,
        "false",
        "true",
        "Enable the metrics endpoint.",
        "flags.go:1074",
    ),
    f(
        "metrics.addr",
        S,
        "127.0.0.1",
        "0.0.0.0",
        "Metrics listen address.",
        "flags.go:1083",
    ),
    f(
        "metrics.port",
        N,
        "6060",
        "6061",
        "Metrics listen port.",
        "flags.go:1088",
    ),
    f(
        "pprof",
        B,
        "false",
        "true",
        "Enable the profiling endpoint.",
        "internal/debug/flags.go:106",
    ),
    f(
        "pprof.addr",
        S,
        "127.0.0.1",
        "0.0.0.0",
        "Profiling listen address.",
        "internal/debug/flags.go:117",
    ),
    f(
        "pprof.port",
        N,
        "6060",
        "6062",
        "Profiling listen port.",
        "internal/debug/flags.go:111",
    ),
    f(
        "version",
        B,
        "false",
        "",
        "Print the version stamp and exit; never opens the datadir. Short form -V.",
        "join.sh:481 (xOneGo lesson)",
    ),
];

fn is_known(name: &str) -> bool {
    FLAGS
        .iter()
        .any(|x| x.name == name || x.aliases.contains(&name) || x.deprecated.contains(&name))
}

/// geth accepts `-flag value`. Rewrite a single-dash long flag to `--flag` when it is a
/// known name (so `-V` and unknown words stay as they are and unknown ones still fail
/// with exit 2). Also returns a warning for each deprecated spelling. Stops at `--`.
pub fn normalise(args: impl IntoIterator<Item = OsString>) -> (Vec<OsString>, Vec<String>) {
    let (mut out, mut warns, mut done) = (vec![], vec![], false);
    for a in args {
        let mut new = None;
        if let (false, Some(s)) = (done, a.to_str()) {
            if s == "--" {
                done = true;
            } else if let Some(body) = s.strip_prefix('-') {
                let dashes = if body.starts_with('-') { 2 } else { 1 };
                let body = &s[dashes..];
                let name = body.split('=').next().unwrap_or(body);
                if name.len() > 1 && is_known(name) {
                    if let Some(x) = FLAGS.iter().find(|x| x.deprecated.contains(&name)) {
                        warns.push(format!("flag -{name} is deprecated, use --{}", x.name));
                    }
                    if dashes == 1 {
                        new = Some(OsString::from(format!("--{body}")));
                    }
                }
            }
        }
        out.push(new.unwrap_or(a));
    }
    (out, warns)
}

fn snapshot() -> Command {
    let net =
        || {
            Arg::new("network").long("network").value_parser(
                clap::builder::PossibleValuesParser::new(xone_chainspec::Network::NAMES),
            )
        };
    let dir = || {
        Arg::new("datadir")
            .long("datadir")
            .required(true)
            .value_name("dir")
    };
    let secs = |n: &'static str| Arg::new(n).long(n).value_name("seconds");
    Command::new("snapshot")
        .about("Export or import an anchored snapshot of the chain state (issue #38).")
        .subcommand_required(true)
        .subcommand(
            Command::new("export")
                .about("Write an anchored snapshot of the datadir's own state at --at (default: the latest committed epoch switch, on V1 the highest sealed block).")
                .arg(dir())
                .arg(Arg::new("out").long("out").required(true).value_name("file"))
                .arg(Arg::new("at").long("at").value_name("block"))
                .arg(secs("max-age"))
                .arg(net()),
        )
        .subcommand(
            Command::new("import")
                .about("Verify a snapshot against the pins and install it into an empty datadir; an interrupted run resumes.")
                .arg(dir())
                .arg(Arg::new("in").long("in").required(true).value_name("file"))
                .arg(Arg::new("pins").long("pins").value_name("file"))
                .arg(secs("max-age-cap"))
                .arg(net()),
        )
}

pub fn command() -> Command {
    let mut c = Command::new("xone")
        .about("XDC xOneRS execution client")
        .disable_version_flag(true)
        .subcommand(Command::new("version").about("Print the version stamp and exit."))
        .subcommand(Command::new("dumpconfig").about("Print the effective config as TOML."))
        .subcommand(
            Command::new("init")
                .about("Write the datadir chain marker from a genesis file and print its hash.")
                .arg(
                    Arg::new("genesis")
                        .value_name("genesis.json")
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("import")
                .about(
                    "Import a geth block file through the staged pipeline with the real executor.",
                )
                .arg(Arg::new("file").value_name("file").required(true))
                .arg(Arg::new("network").long("network").value_parser(
                    clap::builder::PossibleValuesParser::new(xone_chainspec::Network::NAMES),
                )),
        )
        .subcommand(
            Command::new("export")
                .about("Write blocks of the chain imported from --input as a geth block file.")
                .arg(Arg::new("file").value_name("file").required(true))
                .arg(Arg::new("from").value_name("from"))
                .arg(Arg::new("to").value_name("to"))
                .arg(
                    Arg::new("input")
                        .long("input")
                        .required(true)
                        .value_name("blockfile"),
                )
                .arg(Arg::new("network").long("network").value_parser(
                    clap::builder::PossibleValuesParser::new(xone_chainspec::Network::NAMES),
                )),
        )
        .subcommand(snapshot())
        .subcommand(
            Command::new("genesis-hash")
                .about("Print the genesis block hash of a built-in network.")
                .arg(
                    Arg::new("network")
                        .long("network")
                        .required(true)
                        .value_parser(clap::builder::PossibleValuesParser::new(
                            xone_chainspec::Network::NAMES,
                        )),
                ),
        );
    for x in FLAGS {
        let mut a = Arg::new(x.name)
            .long(x.name)
            .aliases(x.aliases.iter().chain(x.deprecated))
            .help(x.meaning);
        a = match x.value {
            Some(v) => a.value_name(v).num_args(1).action(ArgAction::Set),
            // geth bools: `-http`, `-http=false`
            None => a
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("true")
                .action(ArgAction::Set),
        };
        if x.name == "version" {
            a = a.short('V');
        }
        c = c.arg(a);
    }
    c
}

/// The body of `docs/cli/flags.md`.
#[cfg(test)]
pub fn render_doc() -> String {
    let mut s = String::from(
        "# xone flags\n\nGenerated from `bin/xone/src/flags.rs` by the `flags_doc_is_current` test; \
do not edit by hand (`UPDATE_DOCS=1 cargo test -p xone flags_doc` rewrites it).\n\n\
Every flag takes `--name` or `-name` (geth style), `--name value` or `--name=value`. A boolean is \
`--name` or `--name=false`. A flag always wins over the `--config` file. An unknown flag exits 2.\n\n\
Source column: file:line in geth-XDC (`XDCIndia/go-ethereum` at `6fc8e8c2`; `flags.go` is \
`cmd/utils/flags.go`), then the `join.sh` launch lines that pass the flag (ItWorksinMyLocal \
`24b915e`, geth line 570, xone line 682).\n\n\
| Flag | Aliases | Default | Meaning | Source |\n|---|---|---|---|---|\n",
    );
    for x in FLAGS {
        let al: Vec<String> = x
            .aliases
            .iter()
            .map(|a| format!("`-{a}`"))
            .chain(x.deprecated.iter().map(|a| format!("`-{a}` (deprecated)")))
            .collect();
        let name = match x.value {
            Some(v) => format!("`--{} {}`", x.name, v),
            None => format!("`--{}`", x.name),
        };
        let al = if x.name == "version" {
            "`-V`".to_owned()
        } else {
            al.join(", ")
        };
        s += &format!(
            "| {name} | {al} | {} | {} | {} |\n",
            x.default.replace('|', "\\|"),
            x.meaning,
            x.source
        );
    }
    s + "\nSubcommands: `init <genesis.json>`, `version`, `dumpconfig`, `genesis-hash --network <name>`, `import <file>`, `export <file> [from] [to] --input <blockfile>`, `snapshot export --datadir <dir> --out <file> [--at <block>]`, `snapshot import --datadir <dir> --in <file> --pins <file>`.\n"
}
