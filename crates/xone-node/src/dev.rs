//! `--dev` (GAP-0015): a one-validator private XDPoS chain made at start, for dapp development.
//!
//! The chain is the pinned devnet genesis (`genesis/devnet.json`) and its fork table row, with chain id
//! [`CHAIN_ID`], ONE masternode (a key made in memory at start, never written) in the V1 checkpoint list and
//! as the only candidate of the 0x88 contract, as `ops/devnet/gen.py` makes it for the first local mint, and a
//! funded developer account with a well-known key. The XDPoS numbers of the devnet stay (epoch 900, gap 450,
//! V2 from block 2700): a V2 reward needs two epoch switches before it (`reward::v2::locate`) and the V1 double
//! validation needs checkpoints at multiples of 900, so a shorter V1 stops at the first V2 reward
//! (docs/ops/first-mint-local.md). The chain is V1 until block 2700, then V2.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde_json::{json, Value};
use xone_chainspec::{Genesis, Network};
use xone_signer::LocalSigner;

use crate::Config;

/// The chain id of every `--dev` chain (geth `--dev`).
pub const CHAIN_ID: u64 = 1337;

/// The developer account: the first account of the `test test ... junk` mnemonic (Hardhat, Anvil). Its key is
/// public; it exists on dev chains only.
pub const DEV_ACCOUNT: Address =
    alloy_primitives::address!("f39fd6e51aad88f6f4ce6ab8827279cfffb92266");
pub const DEV_ACCOUNT_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
/// 1,000,000,000 XDC.
const DEV_BALANCE: u128 = 1_000_000_000 * 10u128.pow(18);

fn slot(n: U256) -> String {
    format!("0x{}", hex32(n))
}
fn hex32(n: U256) -> String {
    alloy_primitives::hex::encode(n.to_be_bytes::<32>())
}
fn word(a: Address) -> U256 {
    U256::from_be_slice(a.as_slice())
}

/// The genesis json and the fork table (the format of `generated/forks.json`, one network) of a dev chain
/// whose only masternode is `validator`, with a block every `period` seconds.
pub fn chain(validator: Address, period: u64) -> Result<(String, String), String> {
    chain_with(&[validator], period, CHAIN_ID)
}

/// [`chain`] for a private network (issue #223): `validators` are the masternodes, in order, in the
/// V1 checkpoint list (`extraData`) and the candidate list of the 0x88 contract (each its own owner,
/// 10,000,000 XDC cap), as `ops/devnet/gen.py` lays out one. The same inputs give the same bytes.
pub fn chain_with(
    validators: &[Address],
    period: u64,
    chain_id: u64,
) -> Result<(String, String), String> {
    if validators.is_empty() {
        return Err("at least one masternode".into());
    }
    let period = period.max(1);
    let mut g: Value =
        serde_json::from_str(Network::Devnet.genesis_json()).map_err(|e| e.to_string())?;
    g["config"]["chainId"] = json!(chain_id);
    let x = &mut g["config"]["XDPoS"];
    x["period"] = json!(period);
    x["v2"]["config"]["minePeriod"] = json!(period);
    x["v2"]["allConfigs"]["0"]["minePeriod"] = json!(period);
    let signers: String = validators
        .iter()
        .map(|v| alloy_primitives::hex::encode(v.as_slice()))
        .collect();
    g["extraData"] = json!(format!("0x{}{signers}{}", "00".repeat(32), "00".repeat(65)));
    // 0x88: the candidate array (slot 8) holds `validators`; validatorsState[v] (slot 1 mapping) is
    // (owner = v, cap = 10,000,000 XDC). gen.py lines 44-51.
    let v = &mut g["alloc"]["0000000000000000000000000000000000000088"]["storage"];
    v[slot(U256::from(8))] = json!(slot(U256::from(validators.len())));
    let first = U256::from_be_bytes(keccak256(U256::from(8).to_be_bytes::<32>()).0);
    for (i, val) in validators.iter().enumerate() {
        v[slot(first + U256::from(i))] = json!(slot(word(*val)));
        let mut key = [0u8; 64];
        key[..32].copy_from_slice(&word(*val).to_be_bytes::<32>());
        key[32..].copy_from_slice(&U256::from(1).to_be_bytes::<32>());
        let owner = U256::from_be_bytes(keccak256(key).0);
        v[slot(owner)] = json!(slot(word(*val)));
        v[slot(owner + U256::from(1))] = json!(slot(U256::from(10_000_000u128 * 10u128.pow(18))));
    }
    g["alloc"][alloy_primitives::hex::encode(DEV_ACCOUNT.as_slice())] =
        json!({ "balance": format!("{:#x}", U256::from(DEV_BALANCE)) });
    let genesis_json = serde_json::to_string_pretty(&g).map_err(|e| e.to_string())?;

    let mut t: Value =
        serde_json::from_str(xone_chainspec::FORKS_JSON).map_err(|e| e.to_string())?;
    let mut n = t["networks"]["devnet"].take();
    n["chainId"] = json!(chain_id.to_string());
    n["xdpos"]["period"] = json!(period);
    n["xdpos"]["v2"]["allConfigs"]["0"]["minePeriod"] = json!(period);
    let genesis = Genesis::from_json(&genesis_json).map_err(|e| e.to_string())?;
    let table = |n: &Value| json!({ "pin": t["pin"], "networks": { "dev": n } }).to_string();
    let spec = xone_chainspec::load_custom(&table(&n)).map_err(|e| e.to_string())?;
    let hash: B256 = genesis.hash(&spec).map_err(|e| e.to_string())?;
    n["genesis"] =
        json!({ "hash": hash.to_string(), "stateRoot": genesis.state_root().to_string() });
    Ok((genesis_json, table(&n)))
}

/// Why `--dev` is refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DevError {
    #[error("--dev makes its own chain and key; it does not take --{0}")]
    Conflict(&'static str),
    #[error("--dev needs an empty datadir: {0} is not empty (a dev chain cannot be resumed, its validator key lived in memory only)")]
    NotEmpty(String),
    #[error("--dev: {0}")]
    Other(String),
}

/// What `--dev` set up: the validator key (memory only) and the lines to print.
pub struct Started {
    pub key: LocalSigner,
    pub banner: String,
}

/// Checks `cfg` (a `--dev` config), makes the key and the chain, writes the public genesis and fork table
/// to `<datadir>/dev/`, and points `cfg` at them with `--mine`, `--http` and `--nodiscover` on.
/// `datadir_given`: the operator named the datadir; otherwise a new temp dir is used.
pub fn prepare(cfg: &mut Config, datadir_given: bool) -> Result<Started, DevError> {
    let conflicts = [
        ("network", cfg.profile.is_some()),
        ("networkid", cfg.network_id.is_some_and(|n| n != CHAIN_ID)),
        ("override.genesis", cfg.override_genesis.is_some()),
        ("chainspec", cfg.chainspec.is_some()),
        ("keystore", cfg.mining.keystore.is_some()),
        ("password", cfg.mining.password_file.is_some()),
        ("unlock", !cfg.mining.unlock.is_empty()),
        ("syncmode snap", cfg.sync.mode == "snap"),
    ];
    if let Some((name, _)) = conflicts.iter().find(|(_, c)| *c) {
        return Err(DevError::Conflict(name));
    }
    if !datadir_given {
        cfg.datadir = std::env::temp_dir().join(format!("xone-dev-{}", std::process::id()));
    }
    let dd = cfg.datadir.clone();
    if std::fs::read_dir(&dd).is_ok_and(|mut d| d.next().is_some()) {
        return Err(DevError::NotEmpty(dd.display().to_string()));
    }
    let other = |e: &dyn std::fmt::Display| DevError::Other(e.to_string());
    let key = LocalSigner::generate().map_err(|e| other(&e))?;
    let (genesis, table) = chain(key.address(), cfg.dev.period).map_err(|e| other(&e))?;
    let dir = dd.join("dev");
    std::fs::create_dir_all(&dir).map_err(|e| other(&e))?;
    std::fs::write(dir.join("genesis.json"), genesis).map_err(|e| other(&e))?;
    std::fs::write(dir.join("forks.json"), table).map_err(|e| other(&e))?;
    cfg.override_genesis = Some(dir.join("genesis.json"));
    cfg.chainspec = Some(dir.join("forks.json"));
    cfg.mining.mine = true;
    cfg.http.enabled = true;
    cfg.p2p.no_discover = true;
    let banner = format!(
        "dev chain {CHAIN_ID}, a block every {} s, datadir {}\n\
         validator {} (key in memory only; this chain cannot be restarted)\n\
         developer account {DEV_ACCOUNT} with 1,000,000,000 XDC, key {DEV_ACCOUNT_KEY} (a public test key: never use it on a real network)\n\
         http on {}:{}",
        cfg.dev.period,
        dd.display(),
        key.address(),
        cfg.http.addr,
        cfg.http.port,
    );
    Ok(Started { key, banner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_chain_has_one_masternode_and_the_developer_funds() {
        let me = Address::repeat_byte(0x42);
        let (g, t) = chain(me, 1).unwrap_or_else(|e| panic!("{e}"));
        let spec = xone_chainspec::load_custom(&t).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(spec.chain_id, CHAIN_ID);
        assert_eq!(spec.xdpos.period, 1);
        let genesis = Genesis::from_json(&g).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(genesis.hash(&spec).ok(), Some(spec.genesis_hash));
        assert_eq!(genesis.state_root(), spec.genesis_state_root);
        let v: Value = serde_json::from_str(&g).unwrap_or_else(|e| panic!("{e}"));
        let extra = v["extraData"].as_str().unwrap_or_default();
        assert_eq!(extra.len(), 2 + 2 * (32 + 20 + 65));
        assert!(extra.contains(&"42".repeat(20)));
        let s = &v["alloc"]["0000000000000000000000000000000000000088"]["storage"];
        assert_eq!(s[slot(U256::from(8))], json!(slot(U256::from(1))));
        // same inputs, same chain; another validator, another chain
        assert_eq!(chain(me, 1).ok().map(|c| c.1), Some(t.clone()));
        assert_ne!(chain(Address::repeat_byte(1), 1).ok().map(|c| c.1), Some(t));
        assert!(v["alloc"]["f39fd6e51aad88f6f4ce6ab8827279cfffb92266"]["balance"].is_string());
    }

    /// The developer key is the address it claims (the first account of the test-junk mnemonic).
    #[test]
    fn developer_key_matches_its_address() {
        let k = alloy_primitives::hex::decode(DEV_ACCOUNT_KEY).unwrap_or_default();
        let sk = k256::ecdsa::SigningKey::from_slice(&k).unwrap_or_else(|e| panic!("{e}"));
        let p = sk.verifying_key().to_encoded_point(false);
        assert_eq!(
            Address::from_slice(&keccak256(&p.as_bytes()[1..])[12..]),
            DEV_ACCOUNT
        );
    }

    #[test]
    fn prepare_refuses_conflicts_and_a_used_datadir() {
        for (k, v) in [
            ("networkid", "50"),
            ("networkid", "51"),
            ("keystore", "/k.json"),
            ("password", "/pw"),
            ("chainspec", "/f.json"),
            ("override.genesis", "/g.json"),
        ] {
            let mut c = Config::default();
            c.set(k, v).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                matches!(prepare(&mut c, false), Err(DevError::Conflict(_))),
                "{k}"
            );
        }
        let mut c = Config {
            profile: Some(crate::Profile::Mainnet),
            ..Config::default()
        };
        assert_eq!(
            prepare(&mut c, false).err(),
            Some(DevError::Conflict("network"))
        );
        let d = std::env::temp_dir().join(format!("xone-dev-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.join("x"), "").unwrap_or_else(|e| panic!("{e}"));
        let mut c = Config {
            datadir: d.clone(),
            ..Config::default()
        };
        assert!(matches!(prepare(&mut c, true), Err(DevError::NotEmpty(_))));
        std::fs::remove_file(d.join("x")).unwrap_or_else(|e| panic!("{e}"));
        let s = prepare(&mut c, true).unwrap_or_else(|e| panic!("{e}"));
        assert!(c.mining.mine && c.http.enabled && c.p2p.no_discover);
        // the key is never on disk: only the public genesis and fork table are written
        let files: Vec<_> = std::fs::read_dir(d.join("dev"))
            .unwrap_or_else(|e| panic!("{e}"))
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(files.len(), 2);
        assert!(s.banner.contains(&format!("{}", s.key.address())));
        let _ = std::fs::remove_dir_all(&d);
    }
}
