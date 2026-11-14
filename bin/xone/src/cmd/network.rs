//! `xone network init` and `xone dumpgenesis` (issue #223). The work is `xone_node::netgen`.

use std::{path::Path, process::ExitCode};

use clap::ArgMatches;
use xone_chainspec::Network;
use xone_node::netgen::{init, Plan};

use super::fail;

fn num<T: std::str::FromStr>(m: &ArgMatches, k: &str) -> Result<T, String> {
    m.get_one::<String>(k)
        .map_or("", String::as_str)
        .parse()
        .map_err(|_| format!("--{k}: not a number"))
}

pub fn network(m: &ArgMatches) -> ExitCode {
    let Some(("init", s)) = m.subcommand() else {
        return fail("network: init");
    };
    let run = || -> Result<(), String> {
        let out = Path::new(s.get_one::<String>("out").map_or("", String::as_str));
        let plan = Plan {
            masternodes: num(s, "masternodes")?,
            period: num(s, "period")?,
            chain_id: num(s, "chain-id")?,
            base_port: num(s, "base-port")?,
        };
        let made = init(out, &plan)?;
        println!(
            "network {} of {} masternodes, genesis {}",
            out.display(),
            made.masternodes.len(),
            made.genesis_hash
        );
        for (i, (a, e)) in made.masternodes.iter().zip(&made.enodes).enumerate() {
            println!("node{}: masternode {a}, {e}", i + 1);
        }
        println!(
            "start a node: xone --config {}/node1/config.toml",
            out.display()
        );
        println!("to mine, import node<k>/validator.key into a keystore and pass --mine --keystore --password");
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

pub fn dumpgenesis(m: &ArgMatches) -> ExitCode {
    match m
        .get_one::<String>("network")
        .map_or("", String::as_str)
        .parse::<Network>()
    {
        Ok(n) => {
            println!("{}", n.genesis_json().trim_end());
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}
