//! The datadir chain marker. `init` and the first start write it; every later start
//! must match it, so a datadir made for chain X cannot be opened as chain Y.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xone_chainspec::{ChainSpec, Genesis, Network};

use crate::Config;

pub const MARKER: &str = "xone-chain.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub chain_id: u64,
    pub genesis_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DatadirError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0} is not a valid chain marker: {1}")]
    BadMarker(PathBuf, String),
    #[error("datadir was initialised for chain {have} (genesis {have_hash}); refusing to use it for chain {want} (genesis {want_hash})")]
    ChainMismatch {
        have: u64,
        have_hash: String,
        want: u64,
        want_hash: String,
    },
    #[error("--networkid {given} does not match the genesis chain id {genesis}")]
    NetworkId { given: u64, genesis: u64 },
    #[error(
        "--chainspec is for private chains; chain id {0} is public and has only its built-in spec"
    )]
    PublicChainspec(u64),
    #[error("genesis json: {0}")]
    GenesisJson(String),
    #[error("chain id {0} has no built-in chain spec; custom chain specs are not supported yet")]
    NoSpec(u64),
    #[error("genesis: {0}")]
    Genesis(#[from] xone_chainspec::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> DatadirError + '_ {
    |source| DatadirError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The built-in network, the parsed genesis and the identity of a genesis file. The hash needs the
/// fork schedule, which only the built-in networks have. The network is found by `config.chainId`; a
/// file without a config block (the devnet551 export) must equal a built-in genesis.
fn identify(genesis_json: &str) -> Result<(Network, Genesis, Marker), DatadirError> {
    let v: serde_json::Value =
        serde_json::from_str(genesis_json).map_err(|e| DatadirError::GenesisJson(e.to_string()))?;
    let genesis = Genesis::from_json(genesis_json)?;
    let net = match v["config"]["chainId"].as_u64() {
        Some(id) => Network::ALL
            .into_iter()
            .find(|n| n.spec().chain_id == id)
            .ok_or(DatadirError::NoSpec(id))?,
        None => Network::ALL
            .into_iter()
            .find(|n| n.genesis().ok().as_ref() == Some(&genesis))
            .ok_or_else(|| DatadirError::GenesisJson("config.chainId is missing".into()))?,
    };
    let spec = net.spec();
    let marker = Marker {
        chain_id: spec.chain_id,
        genesis_hash: genesis.hash(&spec)?.to_string(),
    };
    Ok((net, genesis, marker))
}

/// Chain id and genesis hash of a genesis file.
pub fn identity(genesis_json: &str) -> Result<Marker, DatadirError> {
    identify(genesis_json).map(|(_, _, m)| m)
}

fn read_marker(datadir: &Path) -> Result<Option<Marker>, DatadirError> {
    let path = datadir.join(MARKER);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| DatadirError::BadMarker(path, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&path)(e)),
    }
}

/// The chain marker of a datadir, which must have one.
pub fn marker(datadir: &Path) -> Result<Marker, DatadirError> {
    read_marker(datadir)?.ok_or_else(|| {
        DatadirError::BadMarker(
            datadir.join(MARKER),
            "no chain marker: pass --network".into(),
        )
    })
}

/// Write the marker for `genesis_json`, or check an existing one against it.
pub fn init(datadir: &Path, genesis_json: &str) -> Result<Marker, DatadirError> {
    init_marker(datadir, identity(genesis_json)?)
}

fn init_marker(datadir: &Path, want: Marker) -> Result<Marker, DatadirError> {
    match read_marker(datadir)? {
        Some(have) if have == want => Ok(want),
        Some(have) => Err(DatadirError::ChainMismatch {
            have: have.chain_id,
            have_hash: have.genesis_hash,
            want: want.chain_id,
            want_hash: want.genesis_hash,
        }),
        None => {
            std::fs::create_dir_all(datadir).map_err(io(datadir))?;
            let path = datadir.join(MARKER);
            let text = serde_json::to_string_pretty(&want)
                .map_err(|e| DatadirError::BadMarker(path.clone(), e.to_string()))?;
            std::fs::write(&path, text + "\n").map_err(io(&path))?;
            Ok(want)
        }
    }
}

/// Start-up check: pick the genesis (`--override.genesis`, else the profile's built-in,
/// else XDC mainnet), reject a `--networkid` that disagrees, then [`init`].
pub fn prepare(cfg: &Config) -> Result<Marker, DatadirError> {
    prepare_chain(cfg).map(|(m, _, _)| m)
}

/// [`prepare`], and the chain spec and genesis the node runs. With `--chainspec` the spec is the fork table
/// of a private chain; chain ids 50 and 51 are public and have only their built-in spec.
pub fn prepare_chain(cfg: &Config) -> Result<(Marker, ChainSpec, Genesis), DatadirError> {
    let json = match &cfg.override_genesis {
        Some(p) => std::fs::read_to_string(p).map_err(io(p))?,
        None => cfg
            .profile
            .map_or(Network::Mainnet, |p| p.network())
            .genesis_json()
            .to_owned(),
    };
    let (spec, genesis, marker) = match &cfg.chainspec {
        Some(p) => {
            let table = std::fs::read_to_string(p).map_err(io(p))?;
            let spec = xone_chainspec::load_custom(&table)?;
            if matches!(spec.chain_id, 50 | 51) {
                return Err(DatadirError::PublicChainspec(spec.chain_id));
            }
            let genesis = Genesis::from_json(&json)?;
            let marker = Marker {
                chain_id: spec.chain_id,
                genesis_hash: genesis.hash(&spec)?.to_string(),
            };
            (spec, genesis, marker)
        }
        None => {
            let (net, genesis, marker) = identify(&json)?;
            (net.spec(), genesis, marker)
        }
    };
    if let Some(given) = cfg.network_id.filter(|n| *n != marker.chain_id) {
        return Err(DatadirError::NetworkId {
            given,
            genesis: marker.chain_id,
        });
    }
    Ok((init_marker(&cfg.datadir, marker)?, spec, genesis))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xone-dd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn hash_equals_chainspec_for_every_built_in() {
        for n in Network::ALL {
            let m = identity(n.genesis_json()).unwrap_or_else(|e| panic!("{}: {e}", n.name()));
            assert_eq!(
                m.genesis_hash,
                n.spec().genesis_hash.to_string(),
                "{}",
                n.name()
            );
            assert_eq!(m.chain_id, n.spec().chain_id);
        }
    }

    #[test]
    fn chain_x_datadir_refuses_chain_y() {
        let d = dir("mismatch");
        init(&d, Network::Apothem.genesis_json()).unwrap_or_else(|e| panic!("{e}"));
        // same chain again is fine
        assert!(init(&d, Network::Apothem.genesis_json()).is_ok());
        let err = init(&d, Network::Mainnet.genesis_json()).err();
        assert!(
            matches!(
                err,
                Some(DatadirError::ChainMismatch {
                    have: 51,
                    want: 50,
                    ..
                })
            ),
            "{err:?}"
        );
        let mut cfg = Config {
            datadir: d.clone(),
            profile: Some(crate::config::Profile::Mainnet),
            ..Config::default()
        };
        assert!(matches!(
            prepare(&cfg),
            Err(DatadirError::ChainMismatch { .. })
        ));
        cfg.profile = Some(crate::config::Profile::Apothem);
        cfg.network_id = Some(50);
        assert!(matches!(prepare(&cfg), Err(DatadirError::NetworkId { .. })));
        std::fs::remove_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn unknown_chain_is_a_typed_error() {
        let json = Network::Mainnet
            .genesis_json()
            .replace("\"chainId\": 50", "\"chainId\": 777");
        assert!(matches!(identity(&json), Err(DatadirError::NoSpec(777))));
        assert!(identity("{}").is_err());
    }
}
