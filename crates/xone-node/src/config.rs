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

/// Built-in network selected by `--xdcmainnet`, `--apothem` or `--xdcdevnet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Mainnet,
    Apothem,
    Devnet,
}

impl Profile {
    pub fn of(n: Network) -> Option<Self> {
        match n {
            Network::Mainnet => Some(Self::Mainnet),
            Network::Apothem => Some(Self::Apothem),
            Network::Devnet => Some(Self::Devnet),
            _ => None,
        }
    }

    pub fn network(self) -> Network {
        match self {
            Self::Mainnet => Network::Mainnet,
            Self::Apothem => Network::Apothem,
            Self::Devnet => Network::Devnet,
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
    /// snap, fast or full.
    pub mode: String,
    /// full or archive.
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
    pub unlock: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    /// 0 silent .. 5 detail.
    pub verbosity: u8,
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
    mode: "snap".into(),
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
    unlock: vec![]
});
defaults!(Logging { verbosity: 3 });
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

fn one_of(key: &str, v: &str, ok: &[&str]) -> Result<String, ConfigError> {
    if ok.contains(&v) {
        Ok(v.to_owned())
    } else {
        Err(invalid(key, v, "not an allowed value"))
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))
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
            "ethstats" => self.ethstats = Some(v.to_owned()),
            "xdcmainnet" | "apothem" | "xdcdevnet" => {
                if flag(key, v)? {
                    self.profile = Some(match key {
                        "xdcmainnet" => Profile::Mainnet,
                        "apothem" => Profile::Apothem,
                        _ => Profile::Devnet,
                    });
                }
            }
            "syncmode" => self.sync.mode = one_of(key, v, &["snap", "fast", "full"])?,
            "gcmode" => self.sync.gcmode = one_of(key, v, &["full", "archive"])?,
            "syncfromblock" => self.sync.from_block = Some(num(key, v)?),
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
            "unlock" => self.mining.unlock = list(v),
            "verbosity" => {
                let n: u8 = num(key, v)?;
                if n > 5 {
                    return Err(invalid(key, v, "0 to 5"));
                }
                self.logging.verbosity = n;
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
        assert!(Config::from_toml("bogus = 1").is_err());
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
