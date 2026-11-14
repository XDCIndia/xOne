//! `xone p2p key generate|enode` and `xone p2p bootnode` (issue #178). The work is
//! `xone_node::p2p`; this reads the flags and prints.

use std::{net::SocketAddr, path::Path, process::ExitCode, sync::Arc};

use clap::ArgMatches;
use xone_node::p2p;

use super::fail;

fn arg<'a>(m: &'a ArgMatches, k: &str) -> &'a str {
    m.get_one::<String>(k).map_or("", String::as_str)
}

fn port(m: &ArgMatches, k: &str) -> Result<u16, String> {
    arg(m, k).parse().map_err(|_| format!("--{k}: not a port"))
}

pub fn p2p(m: &ArgMatches) -> ExitCode {
    let run = || -> Result<(), String> {
        match m.subcommand() {
            Some(("key", k)) => match k.subcommand() {
                Some(("generate", s)) => {
                    let key = p2p::generate_key()?;
                    p2p::write_key(Path::new(arg(s, "out")), &key)?;
                    println!(
                        "{}",
                        p2p::enode_url(&key, [127, 0, 0, 1].into(), 30303, 30303)
                    );
                    Ok(())
                }
                Some(("enode", s)) => {
                    let key = p2p::read_key(Path::new(arg(s, "nodekey")))?;
                    let ip = arg(s, "ip")
                        .parse()
                        .map_err(|_| "--ip: not an IP address")?;
                    let tcp = port(s, "port")?;
                    let udp = if s.get_one::<String>("discport").is_some() {
                        port(s, "discport")?
                    } else {
                        tcp
                    };
                    println!("{}", p2p::enode_url(&key, ip, tcp, udp));
                    Ok(())
                }
                _ => Err("p2p key: generate or enode".into()),
            },
            Some(("bootnode", s)) => {
                let key = p2p::read_key(Path::new(arg(s, "nodekey")))?;
                let addr: SocketAddr = arg(s, "addr")
                    .parse()
                    .map_err(|_| "--addr: ip:port expected")?;
                let sock =
                    std::net::UdpSocket::bind(addr).map_err(|e| format!("bind {addr}: {e}"))?;
                let bound = sock.local_addr().map_err(|e| e.to_string())?;
                let ip = if bound.ip().is_unspecified() {
                    [127, 0, 0, 1].into()
                } else {
                    bound.ip()
                };
                println!("{}", p2p::enode_url(&key, ip, bound.port(), bound.port()));
                p2p::bootnode(key, sock, Arc::default())
            }
            _ => Err("p2p: key or bootnode".into()),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
