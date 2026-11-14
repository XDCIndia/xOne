//! `xone network init` (issue #223, first slice): the files of a private XDPoS network of `M`
//! masternodes on one host, from the devnet genesis (the layout of `ops/devnet/gen.py`, through
//! [`crate::dev::chain_with`]).
//!
//! `<out>/genesis.json`, `<out>/forks.json` (for `--override.genesis` and `--chainspec`), and per node
//! `k` (1-based) `<out>/node<k>/config.toml`, `<out>/node<k>/data/nodekey`,
//! `<out>/node<k>/data/static-nodes.json` (every other node) and `<out>/node<k>/validator.key` (the
//! masternode's secret key, hex, mode 0600; it signs nothing until the operator imports it into a
//! keystore). Every node listens on `127.0.0.1`, p2p port `base_port + k - 1`, HTTP `8545 + k - 1`.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, Address};
use secp256k1::{PublicKey, SecretKey, SECP256K1};

use crate::Config;

#[derive(Clone, Debug)]
pub struct Plan {
    pub masternodes: usize,
    pub period: u64,
    pub chain_id: u64,
    pub base_port: u16,
}

/// What was written, for the summary line.
pub struct Made {
    pub masternodes: Vec<Address>,
    pub enodes: Vec<String>,
    pub genesis_hash: String,
}

fn key() -> Result<SecretKey, String> {
    loop {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        if let Ok(k) = SecretKey::from_slice(&b) {
            return Ok(k);
        }
    }
}

/// The account address of a secp256k1 key.
pub fn address_of(k: &SecretKey) -> Address {
    let p = PublicKey::from_secret_key(SECP256K1, k).serialize_uncompressed();
    Address::from_slice(&keccak256(&p[1..])[12..])
}

/// Writes a new owner-only file; refuses to overwrite.
fn secret_file(path: &Path, body: &str) -> Result<(), String> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(body.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, body: &str) -> Result<(), String> {
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes the network of `plan` into `out`, which must not exist or be empty.
pub fn init(out: &Path, plan: &Plan) -> Result<Made, String> {
    if !(1..=64).contains(&plan.masternodes) {
        return Err("--masternodes: 1 to 64".into());
    }
    if usize::from(u16::MAX - plan.base_port) < plan.masternodes {
        return Err("--base-port: too high for that many nodes".into());
    }
    if std::fs::read_dir(out).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{} is not empty", out.display()));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let out = out
        .canonicalize()
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let mut validators = Vec::with_capacity(plan.masternodes);
    let mut node_keys = Vec::with_capacity(plan.masternodes);
    for _ in 0..plan.masternodes {
        validators.push(key()?);
        node_keys.push(key()?);
    }
    let addrs: Vec<Address> = validators.iter().map(address_of).collect();
    let (genesis, table) = crate::dev::chain_with(&addrs, plan.period, plan.chain_id)?;
    let (gpath, tpath) = (out.join("genesis.json"), out.join("forks.json"));
    write(&gpath, &genesis)?;
    write(&tpath, &table)?;
    let ports: Vec<u16> = (0..plan.masternodes)
        .map(|i| plan.base_port + u16::try_from(i).unwrap_or(0))
        .collect();
    let enodes: Vec<String> = node_keys
        .iter()
        .zip(&ports)
        .map(|(k, p)| {
            let id = PublicKey::from_secret_key(SECP256K1, k).serialize_uncompressed();
            format!(
                "enode://{}@127.0.0.1:{p}",
                alloy_primitives::hex::encode(&id[1..])
            )
        })
        .collect();
    let json = |v: &[String]| serde_json::to_string_pretty(v).map_err(|e| e.to_string());
    write(&out.join("static-nodes.json"), &json(&enodes)?)?;
    for (i, (vk, nk)) in validators.iter().zip(&node_keys).enumerate() {
        let dir = out.join(format!("node{}", i + 1));
        let data = dir.join("data");
        std::fs::create_dir_all(&data).map_err(|e| format!("{}: {e}", data.display()))?;
        secret_file(
            &dir.join("validator.key"),
            &alloy_primitives::hex::encode(vk.secret_bytes()),
        )?;
        secret_file(
            &data.join("nodekey"),
            &alloy_primitives::hex::encode(nk.secret_bytes()),
        )?;
        let others: Vec<String> = enodes
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, e)| e.clone())
            .collect();
        write(&data.join("static-nodes.json"), &json(&others)?)?;
        let mut c = Config {
            datadir: data,
            override_genesis: Some(gpath.clone()),
            chainspec: Some(tpath.clone()),
            ..Config::default()
        };
        c.p2p.port = ports.get(i).copied().unwrap_or(plan.base_port);
        c.p2p.no_discover = true;
        c.http.enabled = true;
        c.http.port = 8545 + u16::try_from(i).unwrap_or(0);
        write(
            &dir.join("config.toml"),
            &c.to_toml().map_err(|e| e.to_string())?,
        )?;
    }
    let hash = serde_json::from_str::<serde_json::Value>(&table)
        .ok()
        .and_then(|t| {
            t["networks"]["dev"]["genesis"]["hash"]
                .as_str()
                .map(str::to_owned)
        })
        .ok_or("the fork table has no genesis hash")?;
    Ok(Made {
        masternodes: addrs,
        enodes,
        genesis_hash: hash,
    })
}

/// The node directory of node `k` (1-based) under `out`.
pub fn node_dir(out: &Path, k: usize) -> PathBuf {
    out.join(format!("node{k}"))
}
