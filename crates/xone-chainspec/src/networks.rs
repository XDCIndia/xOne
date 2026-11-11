//! The four built-in XDC networks, ported from geth-XDC `params/config.go`
//! (`XDC{Mainnet,Apothem,Devnet,Devnet551}ChainConfig` and the `*V2Configs`).

use std::{collections::BTreeMap, str::FromStr};

use alloy_primitives::address;

use crate::{
    ChainSpec, Error, ExpTimeoutConfig,
    ForkBlock::{At, Unscheduled},
    Genesis, V2Config, XdposConfig, V2,
};

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

    pub fn spec(self) -> ChainSpec {
        match self {
            Self::Mainnet => mainnet(),
            Self::Apothem => apothem(),
            Self::Devnet => devnet(),
            Self::Devnet551 => devnet551(),
        }
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

/// A v2 entry with the fields every geth entry sets.
fn v2cfg(max_masternodes: u64, cert_threshold: f64, sync: u64, timeout: u64) -> V2Config {
    V2Config {
        max_masternodes,
        cert_threshold,
        timeout_sync_threshold: sync,
        timeout_period: timeout,
        mine_period: 2,
        exp_timeout: ExpTimeoutConfig {
            base: 1.0,
            max_exponent: 0,
        },
        ..Default::default()
    }
}

/// A v2 entry with the node tiers, tiered rewards and penalty parameters
/// (geth v2.8.0+). `tiers` is (protector, observer) caps, `rewards` is
/// (masternode, protector, observer).
fn v2tiered(base: V2Config, tiers: (u64, u64), rewards: (f64, f64, f64)) -> V2Config {
    V2Config {
        max_protector_nodes: tiers.0,
        max_observer_nodes: tiers.1,
        masternode_reward: rewards.0,
        protector_reward: rewards.1,
        observer_reward: rewards.2,
        minimum_miner_block_per_epoch: 5,
        limit_penalty_epoch: 5,
        minimum_signing_tx: 30,
        ..base
    }
}

fn v2(switch_block: u64, switch_epoch: u64, configs: Vec<(u64, V2Config)>) -> Option<V2> {
    Some(V2 {
        switch_block,
        switch_epoch,
        configs: BTreeMap::from_iter(configs),
    })
}

fn xdpos(reward: u64, foundation_wallet: alloy_primitives::Address, v2: Option<V2>) -> XdposConfig {
    XdposConfig {
        period: 2,
        epoch: 900,
        reward,
        reward_checkpoint: 900,
        gap: 450,
        foundation_wallet,
        max_masternodes_v2: 0,
        v2,
    }
}

/// Chain 50.
pub fn mainnet() -> ChainSpec {
    let c = v2cfg;
    let configs = vec![
        (0, c(108, 0.667, 3, 30)),
        (2000, c(108, 0.667, 2, 600)),
        (8000, c(108, 0.667, 2, 60)),
        (220_000, c(108, 0.667, 2, 30)),
        (460_000, c(108, 0.667, 2, 20)),
        (3_200_000, c(108, 0.667, 3, 10)),
        (17_579_700, c(108, 0.667, 3, 60)),
    ];
    ChainSpec {
        homestead: At(1),
        eip150: At(2),
        eip155: At(3),
        eip158: At(3),
        byzantium: At(4),
        constantinople: At(76_321_000),
        petersburg: At(76_321_000),
        istanbul: At(76_321_000),
        berlin: At(76_321_000),
        london: At(76_321_000),
        eip1559: At(98_800_200),
        cancun: At(98_802_000),
        amsterdam: Unscheduled,
        signing_tx_bound: Unscheduled,
        validator_order: Unscheduled,
        parole_dedup: Unscheduled,
        ..ChainSpec::base(
            50,
            xdpos(
                5000,
                address!("92a289fe95a85c53b8d0d113cbaef0c1ec98ac65"),
                v2(80_370_000, 89_300, configs),
            ),
        )
    }
}

/// Chain 51.
pub fn apothem() -> ChainSpec {
    let c = v2cfg;
    let configs = vec![
        (0, c(15, 0.45, 3, 60)),
        (900_000, c(108, 0.667, 3, 60)),
        (20_277_000, c(108, 0.667, 3, 10)),
        (
            27_360_000,
            v2tiered(c(25, 0.667, 3, 10), (3, 2), (63.42, 50.27, 25.13)),
        ),
        (
            27_400_500,
            v2tiered(
                c(108, 0.667, 3, 10),
                (432, 99_999),
                (63.4196, 50.7357, 25.3678),
            ),
        ),
    ];
    ChainSpec {
        homestead: At(1),
        eip150: At(2),
        eip155: At(3),
        eip158: At(3),
        byzantium: At(4),
        constantinople: At(61_290_000),
        petersburg: At(61_290_000),
        istanbul: At(61_290_000),
        berlin: At(61_290_000),
        london: At(61_290_000),
        eip1559: At(71_550_000),
        cancun: At(71_551_800),
        tip_fixed_reward: At(83_600_000),
        tip_upgrade_penalty: At(83_600_000),
        tip_proportional_reward: At(84_733_000),
        gas2500x: At(83_600_000),
        dynamic_gas_limit: At(83_600_000),
        prague: At(84_333_000),
        amsterdam: Unscheduled,
        signing_tx_bound: Unscheduled,
        validator_order: Unscheduled,
        parole_dedup: Unscheduled,
        ..ChainSpec::base(
            51,
            xdpos(
                5000,
                address!("746249c61f5832c5eed53172776b460491bdcd5c"),
                v2(56_828_700, 63_143, configs),
            ),
        )
    }
}

/// Chain 5551.
pub fn devnet() -> ChainSpec {
    let c = v2cfg;
    let configs = vec![
        (0, c(108, 0.667, 3, 10)),
        (
            50_000,
            v2tiered(c(108, 0.667, 3, 10), (324, 2000), (63.42, 50.27, 25.13)),
        ),
    ];
    ChainSpec {
        homestead: At(0),
        eip150: At(0),
        eip155: At(0),
        eip158: At(0),
        byzantium: At(0),
        constantinople: At(0),
        petersburg: At(0),
        istanbul: At(0),
        berlin: At(0),
        london: At(0),
        eip1559: At(25_000),
        cancun: At(25_000),
        prague: At(50_000),
        tip_proportional_reward: At(50_000),
        tip_upgrade_penalty: At(50_000),
        ..ChainSpec::base(
            5551,
            xdpos(
                7125,
                address!("4f288181b1d1aa599c6d7629f1168d46d5f96338"),
                v2(2700, 3, configs),
            ),
        )
    }
}

/// Chain 551.
pub fn devnet551() -> ChainSpec {
    let c = v2cfg;
    let configs = vec![
        (0, c(18, 0.667, 3, 10)),
        (
            45_000,
            v2tiered(c(10, 0.667, 3, 10), (3, 10), (63.42, 50.27, 25.13)),
        ),
        (
            1_683_000,
            v2tiered(c(10, 0.667, 3, 10), (3, 10), (63.4196, 50.7357, 25.3678)),
        ),
    ];
    let mut x = xdpos(
        1026,
        address!("f8b10955abe83acfdae2dbd4b5d351fc5e1c3672"),
        v2(2700, 3, configs),
    );
    x.max_masternodes_v2 = 18;
    ChainSpec {
        homestead: At(0),
        eip150: At(0),
        eip155: At(0),
        eip158: At(0),
        byzantium: At(0),
        constantinople: At(0),
        petersburg: At(0),
        istanbul: At(0),
        berlin: At(0),
        london: At(0),
        merge: At(0),
        shanghai: At(0),
        tip2019: At(0),
        tip_signing: At(0),
        tip_randomize: At(0),
        tip_increase_masternodes: At(0),
        denylist: At(0),
        tip_no_halving_mn_reward: At(0),
        tip_xdcx: At(0),
        tip_xdcx_lending: At(0),
        tip_xdcx_cancellation_fee: At(0),
        tip_trc21_fee: At(0),
        gas50x: At(0),
        gas2500x: At(1_385_100),
        tip_xdcx_miner_disable: At(0),
        tip_xdcx_receiver_disable: At(0),
        eip1559: At(25_000),
        cancun: At(25_000),
        prague: At(50_000),
        dynamic_gas_limit: At(50_000),
        tip_fixed_reward: At(50_000),
        tip_upgrade_penalty: At(50_000),
        ..ChainSpec::base(551, x)
    }
}
