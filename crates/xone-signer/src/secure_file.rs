//! Read a secret file through one open handle: type and mode are checked on the handle
//! that is read (no check-then-open race).

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroizing;

/// Explicit override for the 0600 rule. Passing it is logged at `warn` whenever it is
/// actually needed. There is no flag or environment variable that does the same.
#[derive(Debug, Clone, Copy)]
pub struct AllowInsecurePermissions;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileError {
    #[error("{}: mode {mode:o} is wider than 0600", .path.display())]
    Permissions { path: PathBuf, mode: u32 },
    #[error("{}: i/o error: {kind}", .path.display())]
    Io { path: PathBuf, kind: io::ErrorKind },
    #[error("{}: not a regular file, or larger than {max} bytes", .path.display())]
    Unusable { path: PathBuf, max: u64 },
}

pub(crate) fn read_secret_file(
    path: &Path,
    allow: Option<AllowInsecurePermissions>,
    max: u64,
) -> Result<Zeroizing<Vec<u8>>, FileError> {
    let io_err = |e: io::Error| FileError::Io {
        path: path.to_owned(),
        kind: e.kind(),
    };
    let file = File::open(path).map_err(io_err)?;
    let md = file.metadata().map_err(io_err)?;
    if !md.is_file() || md.len() > max {
        return Err(FileError::Unusable {
            path: path.to_owned(),
            max,
        });
    }
    let mode = md.permissions().mode() & 0o7777;
    if mode & 0o177 != 0 {
        if allow.is_none() {
            return Err(FileError::Permissions {
                path: path.to_owned(),
                mode,
            });
        }
        tracing::warn!(
            path = %path.display(),
            mode = format_args!("{mode:o}"),
            "AllowInsecurePermissions override: loading a key file wider than 0600"
        );
    }
    let mut buf = Zeroizing::new(Vec::new());
    file.take(max + 1).read_to_end(&mut buf).map_err(io_err)?;
    if u64::try_from(buf.len()).map_or(true, |n| n > max) {
        return Err(FileError::Unusable {
            path: path.to_owned(),
            max,
        });
    }
    Ok(buf)
}
