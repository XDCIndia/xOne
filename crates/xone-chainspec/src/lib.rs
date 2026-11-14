//! XDC chain specifications and genesis handling.
//!
//! The fork table is `generated/forks.json`, written by `conformance/exporter`
//! from geth-XDC at the SHA recorded in `genesis/PIN`; nothing in it is typed
//! by hand. The genesis JSON files are byte-exact copies from that pin.

mod denylist;
mod genesis;
mod networks;
mod required;

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};

pub use denylist::{default_denylist_block, is_denylisted, DENYLIST};
pub use genesis::Genesis;
pub use networks::{
    apothem, devnet, devnet551, load, load_custom, load_named, mainnet, net2040, Network,
};
pub use required::{
    check_required, merge_required_blocks, parse_required_blocks, RequiredBlockMismatch,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("required fork block `{0}` is absent from the chain config")]
    MissingFork(&'static str),
    #[error("network `{0}` has no row in the generated fork table")]
    MissingNetwork(&'static str),
    #[error("the generated fork table sets `{0}`, which this crate does not model")]
    UnmodelledFork(String),
    #[error("required key `{0}` is absent from the genesis config")]
    MissingConfig(&'static str),
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

impl Error {
    fn invalid(field: &'static str, value: &str) -> Self {
        Self::InvalidValue {
            field,
            value: value.to_owned(),
        }
    }
}

impl ForkBlock {
    pub fn is_active(self, number: u64) -> bool {
        matches!(self, Self::At(at) if number >= at)
    }

    /// Read one row of the generated table: an absent key is an error, `null`
    /// is geth `nil` ([`ForkBlock::Never`]) and geth's far-future placeholder is
    /// [`ForkBlock::Unscheduled`]. Values are decimal strings.
    fn from_table(
        table: &BTreeMap<String, Option<String>>,
        key: &'static str,
    ) -> Result<Self, Error> {
        match table.get(key).ok_or(Error::MissingFork(key))? {
            None => Ok(Self::Never),
            Some(v) => match v.parse::<u64>() {
                Ok(UNSCHEDULED_BLOCK) => Ok(Self::Unscheduled),
                Ok(n) => Ok(Self::At(n)),
                Err(_) => Err(Error::invalid(key, v)),
            },
        }
    }

    /// Value of a fork a custom genesis must define; absence is a start-up error.
    pub fn required(field: &'static str, block: Option<u64>) -> Result<Self, Error> {
        block.map(Self::At).ok_or(Error::MissingFork(field))
    }
}

/// An error for any non-null key of `table` outside `known`: geth sets a fork
/// this crate has no field for, so it must not be dropped silently.
fn reject_unmodelled(
    table: &BTreeMap<String, Option<String>>,
    known: &[&str],
) -> Result<(), Error> {
    match table
        .iter()
        .find(|(k, v)| v.is_some() && !known.contains(&k.as_str()))
    {
        Some((k, _)) => Err(Error::UnmodelledFork(k.clone())),
        None => Ok(()),
    }
}

// One list drives the struct, the loader and the row listing, so a new fork
// cannot be added to one and forgotten in another. `name = "jsonKey"`.
macro_rules! chain_spec {
    ($($fork:ident = $key:literal),* $(,)?) => {
        /// Chain id, fork schedule and XDPoS parameters of one network.
        #[derive(Debug, Clone, PartialEq)]
        pub struct ChainSpec {
            pub chain_id: u64,
            $(pub $fork: ForkBlock,)*
            /// Activations geth really uses; see [`Effective`].
            pub effective: Effective,
            pub xdpos: XdposConfig,
            /// `Genesis.ToBlock().Hash()` at the pin.
            pub genesis_hash: B256,
            /// `Genesis.ToBlock().Root()` at the pin.
            pub genesis_state_root: B256,
        }

        impl ChainSpec {
            /// The key of every fork row, in declaration order.
            pub const FORK_KEYS: &'static [&'static str] = &[$($key),*];

            /// Every `*Block` row of the generated table must be present.
            fn from_table(
                chain_id: u64,
                blocks: &BTreeMap<String, Option<String>>,
                effective: Effective,
                xdpos: XdposConfig,
                genesis: (B256, B256),
            ) -> Result<Self, Error> {
                reject_unmodelled(blocks, &[$($key),*])?;
                Ok(Self {
                    chain_id,
                    $($fork: ForkBlock::from_table(blocks, $key)?,)*
                    effective,
                    xdpos,
                    genesis_hash: genesis.0,
                    genesis_state_root: genesis.1,
                })
            }

            /// `(name, activation)` for every fork, in declaration order.
            pub fn fork_rows(&self) -> Vec<(&'static str, ForkBlock)> {
                vec![$((stringify!($fork), self.$fork)),*]
            }
        }
    };
}

chain_spec!(
    homestead = "homesteadBlock",
    eip150 = "eip150Block",
    eip155 = "eip155Block",
    eip158 = "eip158Block",
    byzantium = "byzantiumBlock",
    constantinople = "constantinopleBlock",
    petersburg = "petersburgBlock",
    istanbul = "istanbulBlock",
    berlin = "berlinBlock",
    london = "londonBlock",
    eip1559 = "eip1559Block",
    cancun = "cancunBlock",
    prague = "pragueBlock",
    osaka = "osakaBlock",
    amsterdam = "amsterdamBlock",
    tip_fixed_reward = "tipFixedRewardBlock",
    tip_upgrade_penalty = "tipUpgradePenaltyBlock",
    tip_proportional_reward = "tipProportionalRewardBlock",
    gas2500x = "gas2500xBlock",
    dynamic_gas_limit = "dynamicGasLimitBlock",
    signing_tx_bound = "signingTxBoundBlock",
    validator_order = "validatorOrderBlock",
    parole_dedup = "paroleDedupBlock",
    tip_signing = "tipSigningBlock",
    tip2019 = "tip2019Block",
    tip_randomize = "tipRandomizeBlock",
    tip_increase_masternodes = "tipIncreaseMasternodesBlock",
    denylist = "denylistBlock",
    tip_no_halving_mn_reward = "tipNoHalvingMNRewardBlock",
    tip_xdcx = "tipXDCXBlock",
    tip_xdcx_lending = "tipXDCXLendingBlock",
    tip_xdcx_cancellation_fee = "tipXDCXCancellationFeeBlock",
    tip_trc21_fee = "tipTRC21FeeBlock",
    gas50x = "gas50xBlock",
    tip_xdcx_miner_disable = "tipXDCXMinerDisableBlock",
    tip_xdcx_receiver_disable = "tipXDCXReceiverDisableBlock",
    tip_epoch_halving = "tipEpochHalvingBlock",
    merge = "mergeBlock",
    shanghai = "shanghaiBlock",
);

macro_rules! effective {
    ($($field:ident = $key:literal),* $(,)?) => {
        /// First block at which geth's own predicate holds, found by the
        /// exporter. Differs from the raw row where geth does not read the
        /// row alone: a global TIP constant, or a fallback such as
        /// `IsIstanbul` also firing at `TIPXDCXCancellationFee`.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Effective {
            $(pub $field: ForkBlock,)*
        }

        impl Effective {
            fn from_table(table: &BTreeMap<String, Option<String>>) -> Result<Self, Error> {
                reject_unmodelled(table, &[$($key),*])?;
                Ok(Self { $($field: ForkBlock::from_table(table, $key)?,)* })
            }
        }
    };
}

effective!(
    constantinople = "constantinople",
    eip1559 = "eip1559",
    gas50x = "gas50x",
    istanbul = "istanbul",
    petersburg = "petersburg",
    tip2019 = "tip2019",
    tip_no_halving_mn_reward = "tipNoHalvingMNReward",
    tip_randomize = "tipRandomize",
    tip_signing = "tipSigning",
    tip_trc21_fee = "tipTRC21Fee",
    tip_xdcx = "tipXDCX",
    tip_xdcx_cancellation_fee = "tipXDCXCancellationFee",
    tip_xdcx_miner_disable = "tipXDCXMinerDisable",
    tip_xdcx_receiver_disable = "tipXDCXReceiverDisable",
);

/// Rules that geth derives from the table but not from one row. All are pure
/// functions of the table; see `spec/chainspec/non-parameter-switches.md`.
/// The quiet window that mainnet and Apothem were qualified with (ADR-010 section 8: mainnet now
/// `2 * 60 * 1.0`), and the window of a chain without a V2 section.
pub const QUIET_WINDOW_QUALIFIED: u64 = 120;

impl ChainSpec {
    /// ADR-010 section 8: the signer's quiet window in seconds of connected time, a parameter of the
    /// network. `W = 2 * TimeoutPeriod * Base^MaxExponent` of the newest V2 config (the one every
    /// round from here on runs under): a live signer emits a vote or a timeout at least once per
    /// timeout period (geth-XDC `consensus/XDPoS/engines/engine_v2/timeout.go`, G8), so a second
    /// signer on the key is seen inside `W`. Mainnet (50) and Apothem (51) keep at least the
    /// [`QUIET_WINDOW_QUALIFIED`] they were qualified with (Apothem's timeout period is 10 s today,
    /// which gives 20). A chain with no V2 section takes [`QUIET_WINDOW_QUALIFIED`]. Never 0.
    pub fn quiet_window(&self) -> u64 {
        let adr = self
            .xdpos
            .v2
            .as_ref()
            .and_then(|v| v.configs.values().next_back())
            .map(|c| {
                let exp = c
                    .exp_timeout
                    .base
                    .powi(i32::from(c.exp_timeout.max_exponent));
                #[allow(
                    clippy::as_conversions,
                    reason = "f64 to u64 saturates; NaN is 0, refused below"
                )]
                let w = (2.0 * c.timeout_period as f64 * exp).ceil() as u64;
                w
            })
            .filter(|w| *w > 0)
            .unwrap_or(QUIET_WINDOW_QUALIFIED);
        match self.chain_id {
            50 | 51 => adr.max(QUIET_WINDOW_QUALIFIED),
            _ => adr,
        }
    }

    /// geth `IsIstanbul`: the Istanbul row, or `TIPXDCXCancellationFee`.
    pub fn is_istanbul(&self, n: u64) -> bool {
        self.istanbul.is_active(n) || self.effective.tip_xdcx_cancellation_fee.is_active(n)
    }

    /// geth `IsPetersburg`: `TIPXDCXCancellationFee`, the Petersburg row, or
    /// Constantinople when the Petersburg row is `nil`.
    pub fn is_petersburg(&self, n: u64) -> bool {
        self.effective.tip_xdcx_cancellation_fee.is_active(n)
            || self.petersburg.is_active(n)
            || (self.petersburg == ForkBlock::Never && self.constantinople.is_active(n))
    }

    /// geth `IsTIPXDCXReceiver` (`params/config.go:3477-3480`): the window from `TIPXDCX` up to, not
    /// including, `TIPXDCXReceiverDisable`. XDCx transactions are empty transactions inside it.
    pub fn is_tip_xdcx_receiver(&self, n: u64) -> bool {
        self.effective.tip_xdcx.is_active(n)
            && !self.effective.tip_xdcx_receiver_disable.is_active(n)
    }

    /// geth `IsEIP1559`: the `eip1559` row alone. There is no London fallback.
    pub fn is_eip1559(&self, n: u64) -> bool {
        self.eip1559.is_active(n)
    }

    /// geth `Rules.IsEIP2929` on XDC: Eip1559Block, not Berlin.
    pub fn is_eip2929(&self, n: u64) -> bool {
        self.is_eip1559(n)
    }

    /// geth `Rules.IsEIP3860` on XDC: Eip1559Block, not Shanghai.
    pub fn is_eip3860(&self, n: u64) -> bool {
        self.is_eip1559(n)
    }

    /// geth `XDCPreEIP2028Calldata`: 68 gas per non-zero calldata byte before
    /// Prague, and on chain 5551 at every height.
    pub fn is_pre_eip2028_calldata(&self, n: u64) -> bool {
        !self.prague.is_active(n) || self.chain_id == 5551
    }

    /// geth `ApplyMulticall3HardFork`: Multicall3 code is installed on Prague
    /// blocks after genesis, except on chain 5551.
    pub fn installs_multicall3(&self, n: u64) -> bool {
        n != 0 && self.prague.is_active(n) && self.chain_id != 5551
    }
}

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

    /// #118: the ADR-010 window on every built-in network, never 0; mainnet and Apothem keep 120 s.
    #[test]
    fn the_quiet_window_is_the_adr_010_window_of_every_network() {
        let w = |n: Network| n.spec().quiet_window();
        assert_eq!(w(Network::Mainnet), 120);
        assert_eq!(w(Network::Apothem), 120);
        assert_eq!(w(Network::Net2040), 120); // 2 * 60 s
        assert_eq!(w(Network::Devnet), 20); // 2 * 10 s
        assert_eq!(w(Network::Devnet551), 20);
    }

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
    #[allow(clippy::disallowed_methods)] // test-only snapshot bless switch, never in a shipped path
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

    /// XDPoS parameters and every v2 round entry, one file per network, so a
    /// change in `generated/forks.json` shows up as a diff of
    /// `tests/snapshots/<net>.xdpos.txt`. Regenerate with `XONE_BLESS=1 cargo test`.
    #[test]
    #[allow(clippy::disallowed_methods)] // test-only snapshot bless switch, never in a shipped path
    fn xdpos_snapshot() {
        for net in Network::ALL {
            let got = format!("{:#?}\n", net.spec().xdpos);
            let path = format!(
                "{}/tests/snapshots/{}.xdpos.txt",
                env!("CARGO_MANIFEST_DIR"),
                net.name()
            );
            if std::env::var_os("XONE_BLESS").is_some() {
                std::fs::write(&path, &got).unwrap();
            }
            let want = std::fs::read_to_string(&path).unwrap();
            assert_eq!(got, want, "{} xdpos config changed", net.name());
        }
    }

    fn at(f: ForkBlock) -> u64 {
        match f {
            ForkBlock::At(n) => n,
            other => panic!("expected an active fork, got {other:?}"),
        }
    }

    /// `forks.json` with `edit` applied, loaded as `net`.
    fn load_edited(
        net: Network,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> Result<ChainSpec, Error> {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("../generated/forks.json")).unwrap();
        edit(&mut v);
        load(net, &v.to_string())
    }

    #[test]
    fn generated_table_loads_and_matches_genesis() {
        let table: serde_json::Value =
            serde_json::from_str(include_str!("../generated/forks.json")).unwrap();
        assert_eq!(
            table["pin"].as_str().unwrap(),
            include_str!("../genesis/PIN").trim()
        );
        for net in Network::ALL {
            let spec = net.try_spec().unwrap();
            let g = net.genesis().unwrap();
            assert_eq!(g.hash(&spec).unwrap(), spec.genesis_hash, "{}", net.name());
            assert_eq!(g.state_root(), spec.genesis_state_root, "{}", net.name());
        }
    }

    #[test]
    fn placeholder_is_unscheduled_and_null_is_never() {
        let edit = |apothem_amsterdam: serde_json::Value| {
            load_edited(Network::Apothem, |v| {
                v["networks"]["apothem"]["blocks"]["amsterdamBlock"] = apothem_amsterdam;
                v["networks"]["apothem"]["blocks"]["pragueBlock"] = serde_json::Value::Null;
            })
            .unwrap()
        };
        let s = edit("99999999999".into());
        assert_eq!(s.amsterdam, ForkBlock::Unscheduled);
        assert!(!s.amsterdam.is_active(u64::MAX));
        assert_eq!(s.prague, ForkBlock::Never);
        assert_eq!(edit("7".into()).amsterdam, ForkBlock::At(7));
        // One above the placeholder is a real (absurd) height, not the sentinel.
        assert_eq!(
            edit("100000000000".into()).amsterdam,
            ForkBlock::At(100_000_000_000)
        );
    }

    #[test]
    fn damaged_table_is_a_typed_error_not_a_default() {
        let drop_key = |net: &'static str, section: &'static str, key: &'static str| {
            load_edited(Network::Mainnet, |v| {
                v["networks"][net][section]
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
            })
        };
        assert!(matches!(
            drop_key("mainnet", "blocks", "eip1559Block"),
            Err(Error::MissingFork("eip1559Block"))
        ));
        assert!(matches!(
            drop_key("mainnet", "effective", "istanbul"),
            Err(Error::MissingFork("istanbul"))
        ));
        assert!(matches!(
            load_edited(Network::Mainnet, |v| {
                v["networks"].as_object_mut().unwrap().remove("mainnet");
            }),
            Err(Error::MissingNetwork("mainnet"))
        ));
        // geth sets a fork this crate has no field for.
        assert!(matches!(
            load_edited(Network::Mainnet, |v| {
                v["networks"]["mainnet"]["blocks"]["daoForkBlock"] = "5".into();
            }),
            Err(Error::UnmodelledFork(k)) if k == "daoForkBlock"
        ));
        assert!(matches!(
            load_edited(Network::Mainnet, |v| {
                v["networks"]["mainnet"]["blocks"]["londonBlock"] = "0x10".into();
            }),
            Err(Error::InvalidValue {
                field: "londonBlock",
                ..
            })
        ));
        // A field the exporter adds must not be ignored here.
        assert!(matches!(
            load_edited(Network::Mainnet, |v| {
                v["networks"]["mainnet"]["extra"] = 1.into();
            }),
            Err(Error::Json(_))
        ));
    }

    /// Every block a rule can flip at, on each network, from the table itself.
    fn probe_heights(spec: &ChainSpec) -> Vec<u64> {
        let mut forks: Vec<ForkBlock> = spec.fork_rows().into_iter().map(|(_, f)| f).collect();
        let e = &spec.effective;
        forks.extend([
            e.istanbul,
            e.petersburg,
            e.eip1559,
            e.tip_xdcx_cancellation_fee,
        ]);
        let mut hs: Vec<u64> = forks
            .into_iter()
            .filter_map(|f| match f {
                ForkBlock::At(n) => Some([n.saturating_sub(1), n, n + 1]),
                _ => None,
            })
            .flatten()
            .collect();
        hs.sort_unstable();
        hs.dedup();
        hs
    }

    #[test]
    fn derived_rules_equal_what_geth_exported() {
        for net in Network::ALL {
            let s = net.spec();
            for n in probe_heights(&s) {
                let name = net.name();
                assert_eq!(
                    s.is_istanbul(n),
                    s.effective.istanbul.is_active(n),
                    "{name} istanbul {n}"
                );
                assert_eq!(
                    s.is_petersburg(n),
                    s.effective.petersburg.is_active(n),
                    "{name} petersburg {n}"
                );
                assert_eq!(
                    s.is_eip1559(n),
                    s.effective.eip1559.is_active(n),
                    "{name} eip1559 {n}"
                );
                // Constantinople has no fallback: the row is the rule.
                assert_eq!(
                    s.constantinople.is_active(n),
                    s.effective.constantinople.is_active(n),
                    "{name} constantinople {n}"
                );
            }
        }
    }

    /// `(minus one, exact, plus one)` of `rule` around `boundary`.
    fn edge(rule: impl Fn(u64) -> bool, boundary: u64) -> (bool, bool, bool) {
        (rule(boundary - 1), rule(boundary), rule(boundary + 1))
    }

    #[test]
    fn istanbul_and_petersburg_fire_at_tipxdcx_cancellation_fee() {
        for net in [Network::Mainnet, Network::Apothem] {
            let s = net.spec();
            let b = at(s.effective.tip_xdcx_cancellation_fee);
            // The row is later than the effective block: the fallback is what fires.
            assert!(at(s.istanbul) > b && at(s.petersburg) > b, "{}", net.name());
            assert_eq!(
                edge(|n| s.is_istanbul(n), b),
                (false, true, true),
                "{}",
                net.name()
            );
            assert_eq!(
                edge(|n| s.is_petersburg(n), b),
                (false, true, true),
                "{}",
                net.name()
            );
            // The row itself changes nothing more once the fallback is active.
            let r = at(s.istanbul);
            assert_eq!(
                edge(|n| s.is_istanbul(n), r),
                (true, true, true),
                "{}",
                net.name()
            );
            // Constantinople has no fallback.
            assert_eq!(
                edge(|n| s.constantinople.is_active(n), at(s.constantinople)),
                (false, true, true)
            );
            assert!(!s.constantinople.is_active(b));
        }
    }

    #[test]
    fn eip1559_is_its_own_row_not_london() {
        for net in [Network::Mainnet, Network::Apothem] {
            let s = net.spec();
            let b = at(s.eip1559);
            assert!(at(s.london) < b, "{}", net.name());
            assert_eq!(
                edge(|n| s.is_eip1559(n), b),
                (false, true, true),
                "{}",
                net.name()
            );
            assert_eq!(
                edge(|n| s.is_eip2929(n), b),
                (false, true, true),
                "{}",
                net.name()
            );
            assert_eq!(
                edge(|n| s.is_eip3860(n), b),
                (false, true, true),
                "{}",
                net.name()
            );
            // London alone does not turn the fee market on.
            assert!(!s.is_eip1559(at(s.london)), "{}", net.name());
        }
        // No London fallback when the row is absent.
        let mut s = Network::Apothem.spec();
        s.eip1559 = ForkBlock::Never;
        assert!(!s.is_eip1559(u64::MAX));
    }

    #[test]
    fn calldata_price_exception_for_chain_5551() {
        let s = Network::Apothem.spec();
        assert_eq!(
            edge(|n| s.is_pre_eip2028_calldata(n), at(s.prague)),
            (true, false, false)
        );
        // Mainnet has no Prague yet: 68 gas at every height.
        assert!(Network::Mainnet.spec().is_pre_eip2028_calldata(u64::MAX));
        // Devnet 5551 activates Prague and keeps 68 gas.
        let d = Network::Devnet.spec();
        assert_eq!(
            edge(|n| d.is_pre_eip2028_calldata(n), at(d.prague)),
            (true, true, true)
        );
        let d551 = Network::Devnet551.spec();
        assert!(!d551.is_pre_eip2028_calldata(at(d551.prague)));

        // Multicall3 is installed from Prague on, but never on 5551.
        assert_eq!(
            edge(|n| s.installs_multicall3(n), at(s.prague)),
            (false, true, true)
        );
        assert_eq!(
            edge(|n| d551.installs_multicall3(n), at(d551.prague)),
            (false, true, true)
        );
        assert!(!d.installs_multicall3(at(d.prague)) && !d.installs_multicall3(u64::MAX));
        assert!(!Network::Mainnet.spec().installs_multicall3(u64::MAX));
    }
}
