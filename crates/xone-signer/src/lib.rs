//! Key custody for the producer and the node (issue #40, XB-20).
//!
//! Only the V2 safety unit (`xone-consensus`) and `xone-node` may depend on this crate;
//! `ci/layering.py` enforces it. Keys live in zeroizing types with no `Debug` or `Display`
//! of the bytes, key and password files must be mode 0600, and nothing here logs a secret.

#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("xone-signer checks Unix file modes; it builds on Unix only");

pub mod keystore_dir;
pub mod local_keystore;
pub mod nodekey;
pub mod remote;
pub mod secret;
mod secure_file;

pub use local_keystore::{KeystoreError, LocalSigner};
pub use nodekey::{NodeKey, NodeKeyError};
pub use secret::{KeyBytes, Password};
pub use secure_file::{AllowInsecurePermissions, FileError};
