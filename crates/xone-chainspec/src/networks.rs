//! The four built-in XDC networks, loaded from `generated/forks.json`.

use std::{collections::BTreeMap, str::FromStr};

use alloy_primitives::{Address, B256};
use serde::Deserialize;

use crate::{ChainSpec, Effective, Error, ExpTimeoutConfig, Genesis, V2Config, XdposConfig, V2};

/// Written by `conformance/exporter` from the pinned geth-XDC. Never edit it.
const FORKS_JSON: &str = include_str!("../generated/forks.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Apothem,
    Devnet,
    Devnet551,
}

impl Network {
    pub const ALL: [Network; 4] = [Self::Mainnet, Self::Apothem, Self::Devnet, Self::Devnet551];
    pub const NAMES: [&'static str; 4] = ["mainnet", "apothem", "devnet", "devnet551"];

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// The built-in spec, or the typed error of a damaged generated table.
    pub fn try_spec(self) -> Result<ChainSpec, Error> {
        load(self, FORKS_JSON)
    }

    /// The built-in spec. `generated/forks.json` is part of the build and a
    /// test loads every network from it, so this cannot fail in a shipped binary.
    pub fn spec(self) -> ChainSpec {
        self.try_spec()
            .expect("generated/forks.json is checked by the test suite")
    }

    /// The byte-exact genesis file from the geth-XDC pin.
    pub fn genesis_json(self) -> &'static str {
        match self {
            Self::Mainnet => include_str!("../genesis/mainnet.json"),
            Self::Apothem => include_str!("../genesis/apothem.json"),
            Self::Devnet => include_str!("../genesis/devnet.json"),
            Self::Devnet551 => include_str!("../genesis/devnet551.json"),
        }
    }

    pub fn genesis(self) -> Result<Genesis, Error> {
        Genesis::from_json(self.genesis_json())
    }
}

impl FromStr for Network {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|n| n.name() == s)
            .ok_or_else(|| Error::UnknownNetwork(s.to_owned()))
    }
}

/// Chain 50.
pub fn mainnet() -> ChainSpec {
    Network::Mainnet.spec()
}

/// Chain 51.
pub fn apothem() -> ChainSpec {
    Network::Apothem.spec()
}

/// Chain 5551.
pub fn devnet() -> ChainSpec {
    Network::Devnet.spec()
}

/// Chain 551.
pub fn devnet551() -> ChainSpec {
    Network::Devnet551.spec()
}

// Shapes of generated/forks.json. Unknown keys are an error so that a field
// added to the exporter cannot be ignored here.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[allow(dead_code)] // checked against genesis/PIN by a test
    pin: String,
    networks: BTreeMap<String, RawNetwork>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNetwork {
    chain_id: String,
    blocks: BTreeMap<String, Option<String>>,
    effective: BTreeMap<String, Option<String>>,
    xdpos: RawXdpos,
    genesis: RawGenesis,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawGenesis {
    hash: B256,
    state_root: B256,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawXdpos {
    period: u64,
    epoch: u64,
    reward: u64,
    reward_checkpoint: u64,
    gap: u64,
    foudation_wallet_addr: Address,
    max_masternodes_v2: u64,
    v2: RawV2,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawV2 {
    switch_block: String,
    switch_epoch: u64,
    all_configs: BTreeMap<u64, RawV2Config>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawV2Config {
    // geth leaves it unset on several entries; the map key is authoritative.
    #[allow(dead_code)]
    switch_round: u64,
    mine_period: u64,
    timeout_sync_threshold: u64,
    timeout_period: u64,
    #[serde(rename = "certificateThreshold")]
    cert_threshold: f64,
    max_masternodes: u64,
    max_protector_nodes: u64,
    max_observer_nodes: u64,
    masternode_reward: f64,
    protector_reward: f64,
    observer_reward: f64,
    minimum_miner_block_per_epoch: u64,
    limit_penalty_epoch: u64,
    minimum_signing_tx: u64,
    exp_timeout_config: RawExpTimeout,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawExpTimeout {
    base: f64,
    max_exponent: u8,
}

/// Build the spec of `net` from the text of a generated fork table. A row the
/// table lacks, a value that is not a decimal block number, or a fork this
/// crate does not model is an error; nothing defaults.
pub fn load(net: Network, json: &str) -> Result<ChainSpec, Error> {
    load_named(net.name(), json)
}

/// [`load`] for a table row whose network is not built in (a devnet exported
/// with `conformance/exporter blocks -forks-out`).
pub fn load_named(name: &'static str, json: &str) -> Result<ChainSpec, Error> {
    let mut file: RawFile = serde_json::from_str(json)?;
    let raw = file
        .networks
        .remove(name)
        .ok_or(Error::MissingNetwork(name))?;
    let x = raw.xdpos;
    let switch_block =
        x.v2.switch_block
            .parse()
            .map_err(|_| Error::invalid("v2.switchBlock", &x.v2.switch_block))?;
    let configs =
        x.v2.all_configs
            .into_iter()
            .map(|(round, c)| {
                let cfg = V2Config {
                    mine_period: c.mine_period,
                    timeout_sync_threshold: c.timeout_sync_threshold,
                    timeout_period: c.timeout_period,
                    cert_threshold: c.cert_threshold,
                    max_masternodes: c.max_masternodes,
                    max_protector_nodes: c.max_protector_nodes,
                    max_observer_nodes: c.max_observer_nodes,
                    masternode_reward: c.masternode_reward,
                    protector_reward: c.protector_reward,
                    observer_reward: c.observer_reward,
                    minimum_miner_block_per_epoch: c.minimum_miner_block_per_epoch,
                    limit_penalty_epoch: c.limit_penalty_epoch,
                    minimum_signing_tx: c.minimum_signing_tx,
                    exp_timeout: ExpTimeoutConfig {
                        base: c.exp_timeout_config.base,
                        max_exponent: c.exp_timeout_config.max_exponent,
                    },
                };
                (round, cfg)
            })
            .collect();
    let xdpos = XdposConfig {
        period: x.period,
        epoch: x.epoch,
        reward: x.reward,
        reward_checkpoint: x.reward_checkpoint,
        gap: x.gap,
        foundation_wallet: x.foudation_wallet_addr,
        max_masternodes_v2: x.max_masternodes_v2,
        v2: Some(V2 {
            switch_block,
            switch_epoch: x.v2.switch_epoch,
            configs,
        }),
    };
    let chain_id = raw
        .chain_id
        .parse()
        .map_err(|_| Error::invalid("chainId", &raw.chain_id))?;
    ChainSpec::from_table(
        chain_id,
        &raw.blocks,
        Effective::from_table(&raw.effective)?,
        xdpos,
        (raw.genesis.hash, raw.genesis.state_root),
    )
}
