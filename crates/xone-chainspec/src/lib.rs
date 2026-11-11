//! XDC chain specifications and genesis handling.
//!
//! Values are ported from geth-XDC `params/config.go` at the SHA recorded in
//! `genesis/PIN`; the genesis JSON files are byte-exact copies from that pin.

mod genesis;
mod networks;

use std::collections::BTreeMap;

use alloy_primitives::Address;

pub use genesis::Genesis;
pub use networks::{apothem, devnet, devnet551, mainnet, Network};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("required fork block `{0}` is absent from the chain config")]
    MissingFork(&'static str),
    #[error("unknown network `{0}`")]
    UnknownNetwork(String),
    #[error("invalid genesis json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
    #[error("genesis activates `{0}` at block 0, which is not supported yet")]
    Unsupported(&'static str),
}

/// geth uses this block number for forks that are declared but not scheduled.
pub const UNSCHEDULED_BLOCK: u64 = 99_999_999_999;

/// Activation of a fork by block number.
///
/// There is deliberately no "missing" state: a built-in network states every
/// fork explicitly, and a custom genesis must go through
/// [`ForkBlock::required`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkBlock {
    /// Active from this block on.
    At(u64),
    /// Declared with geth's far-future placeholder (`99_999_999_999`); never active.
    Unscheduled,
    /// Absent from the chain config (geth `nil`); never active.
    Never,
}

impl ForkBlock {
    pub fn is_active(self, number: u64) -> bool {
        matches!(self, Self::At(at) if number >= at)
    }

    /// Value of a fork a custom genesis must define; absence is a start-up error.
    pub fn required(field: &'static str, block: Option<u64>) -> Result<Self, Error> {
        block.map(Self::At).ok_or(Error::MissingFork(field))
    }
}

// One list drives the struct, the all-`Never` base and the row listing, so a
// new fork cannot be added to one and forgotten in another.
macro_rules! chain_spec {
    ($($fork:ident),* $(,)?) => {
        /// Chain id, fork schedule and XDPoS parameters of one network.
        #[derive(Debug, Clone, PartialEq)]
        pub struct ChainSpec {
            pub chain_id: u64,
            $(pub $fork: ForkBlock,)*
            pub xdpos: XdposConfig,
        }

        impl ChainSpec {
            /// Every fork `Never`; built-ins override what geth sets.
            fn base(chain_id: u64, xdpos: XdposConfig) -> Self {
                Self { chain_id, $($fork: ForkBlock::Never,)* xdpos }
            }

            /// `(name, activation)` for every fork, in declaration order.
            pub fn fork_rows(&self) -> Vec<(&'static str, ForkBlock)> {
                vec![$((stringify!($fork), self.$fork)),*]
            }
        }
    };
}

chain_spec!(
    homestead,
    eip150,
    eip155,
    eip158,
    byzantium,
    constantinople,
    petersburg,
    istanbul,
    berlin,
    london,
    eip1559,
    cancun,
    prague,
    amsterdam,
    tip_fixed_reward,
    tip_upgrade_penalty,
    tip_proportional_reward,
    gas2500x,
    dynamic_gas_limit,
    signing_tx_bound,
    validator_order,
    parole_dedup,
    tip_signing,
    tip2019,
    tip_randomize,
    tip_increase_masternodes,
    denylist,
    tip_no_halving_mn_reward,
    tip_xdcx,
    tip_xdcx_lending,
    tip_xdcx_cancellation_fee,
    tip_trc21_fee,
    gas50x,
    tip_xdcx_miner_disable,
    tip_xdcx_receiver_disable,
    tip_epoch_halving,
    merge,
    shanghai,
);

/// XDPoS consensus parameters (geth `XDPoSConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct XdposConfig {
    pub period: u64,
    pub epoch: u64,
    pub reward: u64,
    pub reward_checkpoint: u64,
    pub gap: u64,
    /// JSON key is literally `foudationWalletAddr` in geth.
    pub foundation_wallet: Address,
    /// 0 when geth leaves it unset.
    pub max_masternodes_v2: u64,
    pub v2: Option<V2>,
}

/// XDPoS v2 switch and per-round configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct V2 {
    pub switch_block: u64,
    pub switch_epoch: u64,
    /// Keyed by switch round.
    pub configs: BTreeMap<u64, V2Config>,
}

impl V2 {
    /// The entry with the highest switch round `<= round`.
    pub fn config_at_round(&self, round: u64) -> Option<&V2Config> {
        self.configs.range(..=round).next_back().map(|(_, c)| c)
    }
}

/// geth `V2Config` without `SwitchRound`: geth leaves that field unset on
/// several entries and selects by map key, so the key is authoritative here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct V2Config {
    pub mine_period: u64,
    pub timeout_sync_threshold: u64,
    pub timeout_period: u64,
    pub cert_threshold: f64,
    pub max_masternodes: u64,
    pub max_protector_nodes: u64,
    pub max_observer_nodes: u64,
    pub masternode_reward: f64,
    pub protector_reward: f64,
    pub observer_reward: f64,
    pub minimum_miner_block_per_epoch: u64,
    pub limit_penalty_epoch: u64,
    pub minimum_signing_tx: u64,
    pub exp_timeout: ExpTimeoutConfig,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpTimeoutConfig {
    pub base: f64,
    pub max_exponent: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_block_semantics() {
        assert!(!ForkBlock::At(10).is_active(9));
        assert!(ForkBlock::At(10).is_active(10));
        assert!(ForkBlock::At(10).is_active(11));
        assert!(ForkBlock::At(0).is_active(0));
        assert!(!ForkBlock::Unscheduled.is_active(0));
        assert!(!ForkBlock::Unscheduled.is_active(u64::MAX));
        assert!(!ForkBlock::Never.is_active(u64::MAX));
    }

    #[test]
    fn custom_genesis_missing_fork_is_a_typed_error() {
        assert_eq!(
            ForkBlock::required("eip1559", Some(5)).unwrap(),
            ForkBlock::At(5)
        );
        assert!(matches!(
            ForkBlock::required("eip1559", None),
            Err(Error::MissingFork("eip1559"))
        ));
    }

    #[test]
    fn config_at_round_picks_highest_switch_round_not_above() {
        let v2 = mainnet().xdpos.v2.unwrap();
        // Timeout periods per mainnet entry: 0:30 2000:600 8000:60 220000:30
        // 460000:20 3200000:10 17579700:60.
        let timeout = |r| v2.config_at_round(r).unwrap().timeout_period;
        assert_eq!(timeout(0), 30);
        assert_eq!(timeout(1999), 30);
        assert_eq!(timeout(2000), 600);
        assert_eq!(timeout(2001), 600);
        assert_eq!(timeout(3_199_999), 20);
        assert_eq!(timeout(3_200_000), 10);
        assert_eq!(timeout(17_579_699), 10);
        assert_eq!(timeout(17_579_700), 60);
        assert_eq!(timeout(u64::MAX), 60);

        // Apothem: masternode cap changes across 27,360,000 and 27,400,500.
        let v2 = apothem().xdpos.v2.unwrap();
        let cap = |r| v2.config_at_round(r).unwrap().max_masternodes;
        assert_eq!(cap(27_359_999), 108);
        assert_eq!(cap(27_360_000), 25);
        assert_eq!(cap(27_360_001), 25);
        assert_eq!(cap(27_400_499), 25);
        assert_eq!(cap(27_400_500), 108);
        assert_eq!(cap(27_400_501), 108);
    }

    /// One line per fork so any value change shows up as a diff of
    /// `tests/snapshots/<net>.txt`. Regenerate with `XONE_BLESS=1 cargo test`.
    #[test]
    fn fork_rows_snapshot() {
        for net in Network::ALL {
            let spec = net.spec();
            let mut got = format!("chain_id = {}\n", spec.chain_id);
            for (name, fork) in spec.fork_rows() {
                let v = match fork {
                    ForkBlock::At(n) => n.to_string(),
                    ForkBlock::Unscheduled => "unscheduled".into(),
                    ForkBlock::Never => "never".into(),
                };
                got += &format!("{name} = {v}\n");
            }
            let path = format!(
                "{}/tests/snapshots/{}.txt",
                env!("CARGO_MANIFEST_DIR"),
                net.name()
            );
            if std::env::var_os("XONE_BLESS").is_some() {
                std::fs::write(&path, &got).unwrap();
            }
            let want = std::fs::read_to_string(&path).unwrap();
            assert_eq!(got, want, "{} fork rows changed", net.name());
        }
    }
}
