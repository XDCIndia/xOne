//! `xone snapshot export|import` (issue #38). The work is `xone_node::snapshot`; this reads the
//! flags, the clock and the pins file and prints what happened.

use std::{path::Path, process::ExitCode, time::Instant};

use clap::ArgMatches;
use xone_chainspec::Network;
use xone_node::snapshot::{export_datadir, import_datadir, network_of, short};

use super::{fail, now};

const DAY: u64 = 86_400;

fn arg<'a>(m: &'a ArgMatches, k: &str) -> &'a str {
    m.get_one::<String>(k).map_or("", String::as_str)
}

fn num(m: &ArgMatches, k: &str, default: u64) -> Result<u64, String> {
    m.get_one::<String>(k).map_or(Ok(default), |v| {
        v.parse().map_err(|_| format!("--{k}: not a number"))
    })
}

fn network(m: &ArgMatches, datadir: &Path) -> Result<Network, String> {
    match m.get_one::<String>("network") {
        Some(n) => n.parse::<Network>().map_err(|e| e.to_string()),
        None => network_of(datadir),
    }
}

pub fn snapshot(m: &ArgMatches) -> ExitCode {
    let (name, s) = match m.subcommand() {
        Some(x) => x,
        None => return fail("snapshot: export or import"),
    };
    let datadir = Path::new(arg(s, "datadir"));
    let run = || -> Result<(), String> {
        let net = network(s, datadir)?;
        let spec = net.spec();
        let t0 = Instant::now();
        match name {
            "export" => {
                let at = s
                    .get_one::<String>("at")
                    .map(|v| {
                        v.parse::<u64>()
                            .map_err(|_| "--at: not a number".to_owned())
                    })
                    .transpose()?;
                let genesis = net.genesis().map_err(|e| e.to_string())?;
                let d = export_datadir(
                    &spec,
                    &genesis,
                    datadir,
                    Path::new(arg(s, "out")),
                    at,
                    now(),
                    num(s, "max-age", DAY)?,
                )?;
                println!(
                    "exported block {} {} root {}: {} accounts, {} slots, {} codes, {} headers, {} bodies, {} bytes in {:.1}s",
                    d.at, short(d.hash), short(d.state_root), d.accounts, d.slots, d.codes, d.headers, d.bodies, d.bytes,
                    t0.elapsed().as_secs_f64()
                );
            }
            _ => {
                // the datadir marker, so that a datadir of another chain is refused
                let cfg = xone_node::Config {
                    datadir: datadir.to_owned(),
                    profile: xone_node::Profile::of(net),
                    ..Default::default()
                };
                xone_node::datadir::prepare_chain(&cfg).map_err(|e| e.to_string())?;
                let pins = s.get_one::<String>("pins").map(Path::new);
                let d = import_datadir(
                    &spec,
                    datadir,
                    Path::new(arg(s, "in")),
                    pins,
                    now(),
                    num(s, "max-age-cap", 7 * DAY)?,
                )
                .map_err(|e| e.to_string())?;
                let st = &d.stats;
                println!(
                    "imported block {} {} root {}: {} accounts, {} slots, {} codes, {} headers, {} bodies, {} chunks, {} bytes in {:.1}s{}",
                    d.anchor.number, short(d.anchor.hash), short(d.anchor.state_root),
                    st.accounts, st.slots, st.codes, st.headers, st.bodies, st.chunks, st.bytes,
                    t0.elapsed().as_secs_f64(),
                    st.resumed_from.map_or(String::new(), |b| format!(" (resumed from byte {b})")),
                );
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
