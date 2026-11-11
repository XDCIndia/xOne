//! The devp2p node key file `nodekey` (`spec/keys/node-key.md`).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use alloy_primitives::{hex, B512};
use thiserror::Error;

use crate::local_keystore::LocalSigner;
use crate::secret::KeyBytes;
use crate::secure_file::{read_secret_file, AllowInsecurePermissions, FileError};

pub const NODE_KEY_FILE: &str = "nodekey";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NodeKeyError {
    #[error("malformed node key: {0}")]
    Malformed(&'static str),
    #[error("{}: mode {mode:o} is wider than 0600", .path.display())]
    Permissions { path: PathBuf, mode: u32 },
    #[error("{}: i/o error: {kind}", .path.display())]
    Io { path: PathBuf, kind: io::ErrorKind },
}

impl From<FileError> for NodeKeyError {
    fn from(e: FileError) -> Self {
        match e {
            FileError::Permissions { path, mode } => Self::Permissions { path, mode },
            FileError::Io { path, kind } => Self::Io { path, kind },
            FileError::Unusable { .. } => Self::Malformed("not a regular file, or too large"),
        }
    }
}

/// The node identity. The signer is public to `xone-node`; the bytes are not.
#[derive(Debug)]
pub struct NodeKey {
    signer: LocalSigner,
}

impl NodeKey {
    pub fn signer(&self) -> &LocalSigner {
        &self.signer
    }

    /// The enode id: the 64-byte public key.
    pub fn node_id(&self) -> B512 {
        self.signer.public_key()
    }
}

/// Read `path`: 64 hex characters, then at most two `\r` or `\n` (geth `crypto.LoadECDSA`).
pub fn load(path: &Path, allow: Option<AllowInsecurePermissions>) -> Result<NodeKey, NodeKeyError> {
    let raw = read_secret_file(path, allow, 64 + 2)?;
    let (head, tail) = raw
        .split_at_checked(64)
        .ok_or(NodeKeyError::Malformed("shorter than 64 hex characters"))?;
    if tail.len() > 2 || tail.iter().any(|b| *b != b'\n' && *b != b'\r') {
        return Err(NodeKeyError::Malformed("bytes after the 64 hex characters"));
    }
    let mut key = KeyBytes::zeroed();
    hex::decode_to_slice(head, key.as_mut_bytes())
        .map_err(|_| NodeKeyError::Malformed("not hex"))?;
    LocalSigner::from_key_bytes(&key)
        .map(|signer| NodeKey { signer })
        .map_err(|_| NodeKeyError::Malformed("not a valid secp256k1 scalar"))
}

/// Load `<datadir>/nodekey`. If the file does not exist, create a fresh key with mode 0600
/// (datadir 0700). Any other failure is an error; geth regenerates on every error
/// (`node/config.go:404-406`), xOne never replaces a node identity it could not read.
pub fn load_or_create(
    datadir: &Path,
    allow: Option<AllowInsecurePermissions>,
) -> Result<NodeKey, NodeKeyError> {
    let path = datadir.join(NODE_KEY_FILE);
    match load(&path, allow) {
        Err(NodeKeyError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => {}
        other => return other,
    }
    let io_err = |e: io::Error| NodeKeyError::Io {
        path: path.clone(),
        kind: e.kind(),
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(datadir)
        .map_err(io_err)?;
    let mut key = KeyBytes::zeroed();
    let signer = loop {
        getrandom::fill(key.as_mut_bytes())
            .map_err(|_| NodeKeyError::Malformed("no system randomness"))?;
        if let Ok(s) = LocalSigner::from_key_bytes(&key) {
            break s;
        }
    };
    let mut text = zeroize::Zeroizing::new([0u8; 64]);
    hex::encode_to_slice(key.as_bytes(), &mut text[..])
        .map_err(|_| NodeKeyError::Malformed("hex"))?;
    // Write a private temp file, then hard-link it into place: it never exists half-written
    // at `nodekey`, and of two racing creators exactly one wins.
    let tmp = datadir.join(format!("{NODE_KEY_FILE}.tmp{}", std::process::id()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io_err)?;
    let written = f.write_all(&text[..]).and_then(|()| f.sync_all());
    let linked = written.and_then(|()| std::fs::hard_link(&tmp, &path));
    let removed = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => removed.map_err(io_err)?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return load(&path, allow),
        Err(e) => return Err(io_err(e)),
    }
    tracing::info!(path = %path.display(), "created node key");
    Ok(NodeKey { signer })
}
