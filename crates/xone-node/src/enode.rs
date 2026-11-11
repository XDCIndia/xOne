//! enode URLs and the static-nodes.json / trusted-nodes.json readers.

use std::{fmt, path::Path, str::FromStr};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EnodeError {
    #[error("not an enode:// URL: {0}")]
    Scheme(String),
    #[error("node id must be 128 hex digits: {0}")]
    Id(String),
    #[error("missing or invalid host:port in {0}")]
    HostPort(String),
    #[error("invalid discport in {0}")]
    DiscPort(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PeerFileError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a JSON array of strings: {source}")]
    Json {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
    #[error("{path} entry {index}: {source}")]
    Entry {
        path: std::path::PathBuf,
        index: usize,
        source: EnodeError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enode {
    /// 128 lowercase hex digits (the uncompressed public key without its prefix).
    pub id: String,
    pub host: String,
    pub port: u16,
    pub discport: Option<u16>,
}

impl FromStr for Enode {
    type Err = EnodeError;
    fn from_str(s: &str) -> Result<Self, EnodeError> {
        let rest = s
            .strip_prefix("enode://")
            .ok_or_else(|| EnodeError::Scheme(s.to_owned()))?;
        let (id, rest) = rest
            .split_once('@')
            .ok_or_else(|| EnodeError::HostPort(s.to_owned()))?;
        if id.len() != 128 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EnodeError::Id(s.to_owned()));
        }
        let (addr, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| EnodeError::HostPort(s.to_owned()))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port: u16 = port
            .parse()
            .map_err(|_| EnodeError::HostPort(s.to_owned()))?;
        if host.is_empty() {
            return Err(EnodeError::HostPort(s.to_owned()));
        }
        let discport = query
            .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("discport=")))
            .map(|d| d.parse().map_err(|_| EnodeError::DiscPort(s.to_owned())))
            .transpose()?;
        Ok(Self {
            id: id.to_ascii_lowercase(),
            host: host.to_owned(),
            port,
            discport,
        })
    }
}

impl fmt::Display for Enode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        write!(f, "enode://{}@{}:{}", self.id, host, self.port)?;
        match self.discport {
            Some(d) => write!(f, "?discport={d}"),
            None => Ok(()),
        }
    }
}

/// Read `static-nodes.json` or `trusted-nodes.json` (a JSON array of enode URLs).
/// A missing file is an empty list, as in geth; a bad entry is a typed error.
pub fn read_peer_file(path: &Path) -> Result<Vec<Enode>, PeerFileError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(source) => {
            return Err(PeerFileError::Read {
                path: path.to_owned(),
                source,
            })
        }
    };
    let urls: Vec<String> = serde_json::from_str(&text).map_err(|source| PeerFileError::Json {
        path: path.to_owned(),
        source,
    })?;
    urls.iter()
        .enumerate()
        .map(|(index, u)| {
            u.parse().map_err(|source| PeerFileError::Entry {
                path: path.to_owned(),
                index,
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "5baefc523ad83eccf9021a1ccb7695496cd4bf82fe96c82557e4238869e50bb8ede9cdb2417d6bde1e04ce000a0186bf3f4f08992fea80703a2788c4342ebf34";

    #[test]
    fn parses_and_displays() {
        for url in [
            format!("enode://{ID}@95.217.56.168:30551"),
            format!("enode://{ID}@[::1]:30303?discport=30301"),
        ] {
            let n: Enode = url.parse().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(n.to_string(), url);
        }
    }

    #[test]
    fn typed_errors() {
        assert!(matches!(
            "http://x".parse::<Enode>(),
            Err(EnodeError::Scheme(_))
        ));
        assert!(matches!(
            "enode://ab@1.2.3.4:1".parse::<Enode>(),
            Err(EnodeError::Id(_))
        ));
        assert!(matches!(
            format!("enode://{ID}@host").parse::<Enode>(),
            Err(EnodeError::HostPort(_))
        ));
        assert!(matches!(
            format!("enode://{ID}@h:99999").parse::<Enode>(),
            Err(EnodeError::HostPort(_))
        ));
        assert!(matches!(
            format!("enode://{ID}@h:1?discport=x").parse::<Enode>(),
            Err(EnodeError::DiscPort(_))
        ));
    }

    #[test]
    fn peer_files() {
        let dir = std::env::temp_dir().join(format!("xone-enode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let ok = dir.join("static-nodes.json");
        std::fs::write(&ok, format!("[\"enode://{ID}@1.2.3.4:30303\"]"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_peer_file(&ok).map(|v| v.len()).ok(), Some(1));
        assert_eq!(
            read_peer_file(&dir.join("absent.json"))
                .map(|v| v.len())
                .ok(),
            Some(0)
        );
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "[\"enode://zz@1.2.3.4:1\"]").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            read_peer_file(&bad),
            Err(PeerFileError::Entry { index: 0, .. })
        ));
        std::fs::write(&bad, "{}").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            read_peer_file(&bad),
            Err(PeerFileError::Json { .. })
        ));
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }
}
