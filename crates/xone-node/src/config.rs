//! Typed node configuration (XB-19). Loaded from TOML, then flags are applied on top
//! with [`Config::set`], so a flag always wins over the file. Defaults follow geth for
//! the flag semantics and ADR-007 for the limits.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xone_chainspec::Network;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("unknown flag or config key `{0}`")]
    UnknownKey(String),
    #[error("invalid value `{value}` for `{key}`: {why}")]
    Invalid {
        key: String,
        value: String,
        why: &'static str,
    },
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file: {0}")]
    Toml(String),
}

/// Built-in network selected by `--xdcmainnet`, `--apothem`, `--xdcdevnet`, `--net2040` or `--network`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Mainnet,
    Apothem,
    Devnet,
    Net2040,
}

impl Profile {
    pub fn of(n: Network) -> Option<Self> {
        match n {
            Network::Mainnet => Some(Self::Mainnet),
            Network::Apothem => Some(Self::Apothem),
            Network::Devnet => Some(Self::Devnet),
            Network::Net2040 => Some(Self::Net2040),
            _ => None,
        }
    }

    pub fn network(self) -> Network {
        match self {
            Self::Mainnet => Network::Mainnet,
            Self::Apothem => Network::Apothem,
            Self::Devnet => Network::Devnet,
            Self::Net2040 => Network::Net2040,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// None: the built-in mainnet unless `override_genesis` or `network_id` is set.
    pub profile: Option<Profile>,
    pub network_id: Option<u64>,
    pub datadir: PathBuf,
    pub identity: Option<String>,
    pub override_genesis: Option<PathBuf>,
    /// Fork table of a private chain (`--chainspec`); the table format of `generated/forks.json` with one network.
    pub chainspec: Option<PathBuf>,
    /// `name:secret@host:port`, as geth.
    pub ethstats: Option<String>,
    pub sync: SyncCfg,
    pub p2p: P2p,
    pub http: Http,
    pub ws: Ws,
    pub authrpc: AuthRpc,
    pub ipc: Ipc,
    pub mining: Mining,
    pub logging: Logging,
    pub metrics: Listen,
    pub pprof: Listen,
    pub limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncCfg {
    /// full or snap; fast is stored as snap (`sync_mode`).
    pub mode: String,
    /// `--snap.pins`: a pins file (`<number> <0x hash>` per line, the `snapshot import --pins`
    /// form) that pins the pivot's epoch-switch header; without it two peers must serve the same
    /// header (`spec/snap/pivot.md`).
    pub snap_pins: Option<PathBuf>,
    /// `--snap.gap-rpc`: URL of any archive RPC that serves `eth_getProof`. A snap sync from geth-XDC
    /// peers reads the 0x88 state of the gap blocks below the pivot from it; every proof is verified
    /// against the header, so the RPC is not trusted (`spec/snap/pivot.md`).
    pub snap_gap_rpc: Option<String>,
    /// `--state.disk`: read the head state from the MDBX tables behind bounded caches instead of
    /// holding every row in memory (`docs/ops/disk-state.md`). Off by default.
    pub state_disk: bool,
    /// full; archive is refused (`gc_mode`).
    pub gcmode: String,
    pub from_block: Option<u64>,
    /// Start from the anchor of an imported snapshot and keep only the retention window.
    pub minimal: bool,
    pub cache_mb: u32,
    /// Blocks of transaction index kept; 0 is the whole chain.
    pub tx_history: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct P2p {
    pub port: u16,
    /// None: same as `port`.
    pub discovery_port: Option<u16>,
    pub max_peers: u32,
    pub no_discover: bool,
    pub nat: Option<String>,
    pub bootnodes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Http {
    pub enabled: bool,
    pub addr: String,
    pub port: u16,
    pub api: Vec<String>,
    pub cors: Vec<String>,
    pub vhosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ws {
    pub enabled: bool,
    pub addr: String,
    pub port: u16,
    pub api: Vec<String>,
    pub origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthRpc {
    pub addr: String,
    pub port: u16,
    pub jwt_secret: Option<PathBuf>,
    pub vhosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ipc {
    pub disabled: bool,
    /// None: `<datadir>/xone.ipc`.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mining {
    pub mine: bool,
    pub etherbase: Option<String>,
    /// File with the account password. The password itself is never a flag or a key.
    pub password_file: Option<PathBuf>,
    /// Keystore file of the validator key (`--keystore`). Block production needs it and the password file.
    pub keystore: Option<PathBuf>,
    pub unlock: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    /// 0 silent .. 5 detail.
    pub verbosity: u8,
    /// `text` or `json` (one object per line).
    pub format: String,
    /// Per-target verbosity, `xone_net=4,xone_sync::snap=5`.
    pub vmodule: String,
}

impl Logging {
    /// The logger's level table and format; `Config::set` and `from_toml` have checked both.
    pub fn levels(
        &self,
    ) -> Result<(crate::observe::logs::LevelCfg, crate::observe::logs::Format), ConfigError> {
        let modules = crate::observe::logs::parse_vmodule(&self.vmodule)
            .map_err(|w| invalid("log.vmodule", &self.vmodule, w))?;
        let format = self
            .format
            .parse()
            .map_err(|w| invalid("log.format", &self.format, w))?;
        Ok((
            crate::observe::logs::LevelCfg {
                verbosity: self.verbosity,
                modules,
            },
            format,
        ))
    }
}

/// A side listener (metrics, pprof).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Listen {
    pub enabled: bool,
    pub addr: String,
    pub port: u16,
}

/// ADR-007 proposals. TOML only: none of these has a geth flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Whole shutdown budget (XB-19: 5 s).
    pub stop_timeout_ms: u64,
    /// Budget of one service inside it.
    pub service_stop_timeout_ms: u64,
    pub rpc_http_connections: u32,
    pub rpc_http_waiting: u32,
    pub rpc_http_in_flight: u32,
    pub rpc_ws_connections: u32,
    pub rpc_ipc_connections: u32,
    pub rpc_body_bytes: u64,
    pub rpc_batch_items: u32,
    pub rpc_response_bytes: u64,
    pub log_queue_lines: u32,
    /// Disk watermark (ADR-007 section 7): the larger of the percent of the datadir filesystem and the
    /// byte floor wins. Import stops below `stop`, resumes above `resume`, the node exits below `exit`.
    pub disk_stop_pct: u8,
    pub disk_stop_min_bytes: u64,
    pub disk_resume_pct: u8,
    pub disk_resume_min_bytes: u64,
    pub disk_exit_pct: u8,
    pub disk_exit_min_bytes: u64,
    /// Seconds between two samples of free space. At least 1.
    pub disk_sample_secs: u32,
}

macro_rules! defaults {
    ($t:ty { $($f:ident : $v:expr),* $(,)? }) => {
        impl Default for $t {
            fn default() -> Self { Self { $($f: $v),* } }
        }
    };
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

defaults!(Config {
    profile: None,
    network_id: None,
    datadir: PathBuf::from("xone-data"),
    identity: None,
    override_genesis: None,
    chainspec: None,
    ethstats: None,
    sync: SyncCfg::default(),
    p2p: P2p::default(),
    http: Http::default(),
    ws: Ws::default(),
    authrpc: AuthRpc::default(),
    ipc: Ipc::default(),
    mining: Mining::default(),
    logging: Logging::default(),
    metrics: Listen::default(),
    pprof: Listen::default(),
    limits: Limits::default(),
});
defaults!(SyncCfg {
    mode: "full".into(),
    snap_pins: None,
    snap_gap_rpc: None,
    state_disk: false,
    gcmode: "full".into(),
    from_block: None,
    minimal: false,
    cache_mb: 4096,
    tx_history: 2_350_000
});
defaults!(P2p {
    port: 30303,
    discovery_port: None,
    max_peers: 50,
    no_discover: false,
    nat: None,
    bootnodes: vec![]
});
defaults!(Http {
    enabled: false,
    addr: "localhost".into(),
    port: 8545,
    api: strs(&["eth", "net", "web3"]),
    cors: vec![],
    vhosts: strs(&["localhost"])
});
defaults!(Ws {
    enabled: false,
    addr: "localhost".into(),
    port: 8546,
    api: strs(&["eth", "net", "web3"]),
    origins: vec![]
});
defaults!(AuthRpc {
    addr: "localhost".into(),
    port: 8551,
    jwt_secret: None,
    vhosts: strs(&["localhost"])
});
defaults!(Listen {
    enabled: false,
    addr: "127.0.0.1".into(),
    port: 6060
});
defaults!(Ipc {
    disabled: false,
    path: None
});
defaults!(Mining {
    mine: false,
    etherbase: None,
    password_file: None,
    keystore: None,
    unlock: vec![]
});
defaults!(Logging {
    verbosity: 3,
    format: "text".into(),
    vmodule: String::new()
});
defaults!(Limits {
    stop_timeout_ms: 5_000,
    service_stop_timeout_ms: 2_000,
    rpc_http_connections: 256,
    rpc_http_waiting: 128,
    rpc_http_in_flight: 64,
    rpc_ws_connections: 128,
    rpc_ipc_connections: 16,
    rpc_body_bytes: 5 << 20,
    rpc_batch_items: 100,
    rpc_response_bytes: 20 << 20,
    log_queue_lines: 8_192,
    // Safer than the ADR-007 proposals (10% or 20 GiB, 15% or 30 GiB, 2% or 4 GiB), which would halt
    // import on a host that has 40 GB free of 420 GB: the owner raises these per fleet.
    disk_stop_pct: 2,
    disk_stop_min_bytes: 2 << 30,
    disk_resume_pct: 3,
    disk_resume_min_bytes: 3 << 30,
    disk_exit_pct: 1,
    disk_exit_min_bytes: 1 << 30,
    disk_sample_secs: 5,
});

fn invalid(key: &str, value: &str, why: &'static str) -> ConfigError {
    ConfigError::Invalid {
        key: key.to_owned(),
        value: value.to_owned(),
        why,
    }
}

fn num<T: std::str::FromStr>(key: &str, v: &str) -> Result<T, ConfigError> {
    v.parse()
        .map_err(|_| invalid(key, v, "not a number in range"))
}

/// geth bool flags: `-http`, `-http=true`, `-http=false`.
fn flag(key: &str, v: &str) -> Result<bool, ConfigError> {
    match v {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(invalid(key, v, "expected true or false")),
    }
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `full`, or `snap` (state download at a pivot, `services::snapsync`). geth-XDC runs `fast` as
/// snap (`eth/downloader/modes.go`: `FastSync` is an alias of `SnapSync`), so does this node: the
/// value is stored as `snap` and the start logs it.
fn sync_mode(key: &str, v: &str) -> Result<String, ConfigError> {
    match v {
        "full" | "snap" => Ok(v.to_owned()),
        "fast" => {
            eprintln!("--{key} fast is an alias of snap, as in geth-XDC: syncing in snap mode");
            Ok("snap".to_owned())
        }
        _ => Err(invalid(key, v, "not an allowed value")),
    }
}

/// The node keeps a 128-block state window and its newest checkpoints whatever `gcmode` says, so
/// `archive` is refused rather than reported to the board as a mode the node does not run.
fn gc_mode(key: &str, v: &str) -> Result<String, ConfigError> {
    match v {
        "full" => Ok(v.to_owned()),
        "archive" => Err(invalid(
            key,
            v,
            "not implemented in this build (historical state is pruned to the state window): use full",
        )),
        _ => Err(invalid(key, v, "not an allowed value")),
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let c: Self = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        let mut c = c;
        c.sync.mode = sync_mode("sync.mode", &c.sync.mode)?;
        gc_mode("sync.gcmode", &c.sync.gcmode)?;
        c.logging.levels()?;
        Ok(c)
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&text)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    /// Apply one flag by its geth name. Bool flags take "true" or "false".
    pub fn set(&mut self, key: &str, v: &str) -> Result<(), ConfigError> {
        match key {
            "datadir" => self.datadir = v.into(),
            "networkid" => self.network_id = Some(num(key, v)?),
            "identity" => self.identity = Some(v.to_owned()),
            "override.genesis" => self.override_genesis = Some(v.into()),
            "chainspec" => self.chainspec = Some(v.into()),
            "ethstats" => self.ethstats = Some(v.to_owned()),
            "xdcmainnet" | "apothem" | "xdcdevnet" | "net2040" => {
                if flag(key, v)? {
                    self.profile = Some(match key {
                        "xdcmainnet" => Profile::Mainnet,
                        "apothem" => Profile::Apothem,
                        "net2040" => Profile::Net2040,
                        _ => Profile::Devnet,
                    });
                }
            }
            "network" => {
                self.profile = Some(match v {
                    "mainnet" => Profile::Mainnet,
                    "apothem" => Profile::Apothem,
                    "devnet" => Profile::Devnet,
                    "net2040" => Profile::Net2040,
                    _ => {
                        return Err(ConfigError::Invalid {
                            key: key.to_owned(),
                            value: v.to_owned(),
                            why: "one of mainnet, apothem, devnet, net2040",
                        })
                    }
                });
            }
            "syncmode" => self.sync.mode = sync_mode(key, v)?,
            "gcmode" => self.sync.gcmode = gc_mode(key, v)?,
            "syncfromblock" => self.sync.from_block = Some(num(key, v)?),
            "snap.pins" => self.sync.snap_pins = Some(v.into()),
            "snap.gap-rpc" => self.sync.snap_gap_rpc = Some(v.to_owned()),
            "state.disk" => self.sync.state_disk = flag(key, v)?,
            "minimal" => self.sync.minimal = flag(key, v)?,
            "cache" => self.sync.cache_mb = num(key, v)?,
            "history.transactions" | "txlookuplimit" => self.sync.tx_history = num(key, v)?,
            "port" => self.p2p.port = num(key, v)?,
            "discovery.port" => self.p2p.discovery_port = Some(num(key, v)?),
            "maxpeers" => self.p2p.max_peers = num(key, v)?,
            "nodiscover" => self.p2p.no_discover = flag(key, v)?,
            "nat" => self.p2p.nat = Some(v.to_owned()),
            "bootnodes" => self.p2p.bootnodes = list(v),
            "http" => self.http.enabled = flag(key, v)?,
            "http.addr" => self.http.addr = v.to_owned(),
            "http.port" => self.http.port = num(key, v)?,
            "http.api" => self.http.api = list(v),
            "http.corsdomain" => self.http.cors = list(v),
            "http.vhosts" => self.http.vhosts = list(v),
            "ws" => self.ws.enabled = flag(key, v)?,
            "ws.addr" => self.ws.addr = v.to_owned(),
            "ws.port" => self.ws.port = num(key, v)?,
            "ws.api" => self.ws.api = list(v),
            "ws.origins" => self.ws.origins = list(v),
            "authrpc.addr" => self.authrpc.addr = v.to_owned(),
            "authrpc.port" => self.authrpc.port = num(key, v)?,
            "authrpc.jwtsecret" => self.authrpc.jwt_secret = Some(v.into()),
            "authrpc.vhosts" => self.authrpc.vhosts = list(v),
            "ipcdisable" => self.ipc.disabled = flag(key, v)?,
            "ipcpath" => self.ipc.path = Some(v.into()),
            "mine" => self.mining.mine = flag(key, v)?,
            "miner.etherbase" => self.mining.etherbase = Some(v.to_owned()),
            "password" => self.mining.password_file = Some(v.into()),
            "keystore" => self.mining.keystore = Some(v.into()),
            "unlock" => self.mining.unlock = list(v),
            "verbosity" => {
                let n: u8 = num(key, v)?;
                if n > 5 {
                    return Err(invalid(key, v, "0 to 5"));
                }
                self.logging.verbosity = n;
            }
            "log.format" => {
                self.logging.format = v.to_owned();
                self.logging.levels()?;
            }
            "log.vmodule" | "vmodule" => {
                self.logging.vmodule = v.to_owned();
                self.logging.levels()?;
            }
            "metrics" => self.metrics.enabled = flag(key, v)?,
            "metrics.addr" => self.metrics.addr = v.to_owned(),
            "metrics.port" => self.metrics.port = num(key, v)?,
            "pprof" => self.pprof.enabled = flag(key, v)?,
            "pprof.addr" => self.pprof.addr = v.to_owned(),
            "pprof.port" => self.pprof.port = num(key, v)?,
            _ => return Err(ConfigError::UnknownKey(key.to_owned())),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_load_round_trip() {
        let mut c = Config::default();
        c.set("apothem", "true").unwrap_or_else(|e| panic!("{e}"));
        for (k, v) in [
            ("datadir", "/d"),
            ("networkid", "51"),
            ("http", "true"),
            ("http.api", "eth,net,XDPoS"),
            ("ethstats", "n:s@h:443"),
            ("discovery.port", "30304"),
            ("unlock", "0xaa,0xbb"),
        ] {
            c.set(k, v).unwrap_or_else(|e| panic!("{e}"));
        }
        let text = c.to_toml().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(Config::from_toml(&text).ok(), Some(c));
        // the default round-trips too (None fields are absent from the file)
        let d = Config::default();
        let text = d.to_toml().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(Config::from_toml(&text).ok(), Some(d));
    }

    #[test]
    fn partial_file_keeps_defaults_and_flag_wins() {
        let mut c = Config::from_toml("[http]\nport = 9000\nenabled = true\n")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.http.port, 9000);
        assert_eq!(c.p2p.port, 30303);
        c.set("http.port", "9100").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.http.port, 9100);
    }

    #[test]
    fn typed_errors() {
        let mut c = Config::default();
        assert!(matches!(
            c.set("nope", "1"),
            Err(ConfigError::UnknownKey(_))
        ));
        assert!(matches!(
            c.set("port", "70000"),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(matches!(
            c.set("syncmode", "light"),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(matches!(
            c.set("verbosity", "6"),
            Err(ConfigError::Invalid { .. })
        ));
        for (k, v) in [("log.format", "xml"), ("log.vmodule", "xone_net=9")] {
            assert!(
                matches!(c.set(k, v), Err(ConfigError::Invalid { .. })),
                "{k}"
            );
        }
        assert!(Config::from_toml("[logging]\nformat = \"xml\"\n").is_err());
        assert!(Config::from_toml("bogus = 1").is_err());
    }

    /// Modes the node does not run are refused by the flag and the config file alike, never accepted
    /// and then synced in full from genesis; fast is snap.
    #[test]
    fn sync_modes() {
        let mut c = Config::default();
        let e = c.set("gcmode", "archive").unwrap_err().to_string();
        assert!(e.contains("not implemented in this build"), "{e}");
        for (v, want) in [("snap", "snap"), ("fast", "snap")] {
            c.set("syncmode", v).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(c.sync.mode, want);
        }
        assert_eq!(
            Config::from_toml("[sync]\nmode = \"fast\"\n")
                .unwrap_or_else(|e| panic!("{e}"))
                .sync
                .mode,
            "snap"
        );
        c.set("syncmode", "full").unwrap_or_else(|e| panic!("{e}"));
        c.set("gcmode", "full").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (c.sync.mode.as_str(), c.sync.gcmode.as_str()),
            ("full", "full")
        );
        assert_eq!(Config::default().sync.mode, "full");
        for t in [
            "[sync]\nmode = \"light\"\n",
            "[sync]\ngcmode = \"archive\"\n",
        ] {
            assert!(
                matches!(Config::from_toml(t), Err(ConfigError::Invalid { .. })),
                "{t}"
            );
        }
    }

    /// Every built-in network with a profile maps back to itself: `snapshot import --network net2040`
    /// once initialised the datadir for mainnet (no Net2040 arm).
    #[test]
    fn every_profile_round_trips_its_network() {
        for n in Network::ALL {
            if let Some(p) = Profile::of(n) {
                assert_eq!(p.network(), n);
            }
        }
        assert_eq!(Profile::of(Network::Net2040), Some(Profile::Net2040));
    }

    #[test]
    fn disk_watermark_defaults_are_ordered_and_misorder_is_refused() {
        let l = Limits::default();
        assert_eq!(
            (l.disk_stop_pct, l.disk_resume_pct, l.disk_exit_pct),
            (2, 3, 1)
        );
        assert_eq!(l.disk_exit_min_bytes, 1 << 30);
        assert!(crate::DiskWatermark::from_limits(400 << 30, &l).is_some());
        let bad = "[limits]\ndisk_stop_pct = 1\n";
        let c = Config::from_toml(bad).unwrap();
        assert!(crate::DiskWatermark::from_limits(1 << 40, &c.limits).is_none());
        assert!(Config::from_toml("[limits]\ndisk_nope = 1\n").is_err());
    }

    #[test]
    fn limits_follow_adr_007() {
        let l = Limits::default();
        assert_eq!(
            (l.stop_timeout_ms, l.rpc_http_connections, l.rpc_batch_items),
            (5000, 256, 100)
        );
        assert_eq!(l.rpc_body_bytes, 5 * 1024 * 1024);
    }
}
