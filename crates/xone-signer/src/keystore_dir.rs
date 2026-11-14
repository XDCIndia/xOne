//! A geth keystore directory (`xone account new | import | list | update`): keystore v3 files
//! with scrypt (geth `accounts/keystore/passphrase.go` `EncryptKey`), named
//! `UTC--<time>--<address>` (geth `keyFileName`), mode 0600 in a 0700 directory.

use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use alloy_primitives::{hex, keccak256, Address};
use ctr::cipher::{KeyIvInit, StreamCipher};
use serde_json::json;
use zeroize::Zeroizing;

use crate::{
    local_keystore::{decrypt_key, KeystoreError, LocalSigner},
    secret::{KeyBytes, Password},
};

/// scrypt cost of a written file: geth `StandardScryptN`/`P` (256 MiB, about a second) or, with
/// `--lightkdf`, `LightScryptN`/`P` (4 MiB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kdf {
    Standard,
    Light,
}

impl Kdf {
    /// (log2 N, r, p).
    fn params(self) -> (u8, u32, u32) {
        match self {
            Kdf::Standard => (18, 8, 1),
            Kdf::Light => (12, 8, 6),
        }
    }
}

const MAX_KEYFILE: u64 = 4096;

fn io_err(path: &Path) -> impl Fn(io::Error) -> KeystoreError + '_ {
    move |e| KeystoreError::Io {
        path: path.to_owned(),
        kind: e.kind(),
    }
}

fn random<const N: usize>() -> Result<[u8; N], KeystoreError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| KeystoreError::Malformed("no system randomness"))?;
    Ok(b)
}

/// A new random key (a valid secp256k1 scalar).
pub fn generate() -> Result<(KeyBytes, Address), KeystoreError> {
    let mut key = KeyBytes::zeroed();
    loop {
        getrandom::fill(key.as_mut_bytes())
            .map_err(|_| KeystoreError::Malformed("no system randomness"))?;
        if let Ok(s) = LocalSigner::from_key_bytes(&key) {
            return Ok((key, s.address()));
        }
    }
}

/// geth `crypto.LoadECDSA`: 64 hex characters, then only whitespace.
pub fn parse_hex_key(text: &[u8]) -> Result<(KeyBytes, Address), KeystoreError> {
    let bad = KeystoreError::Malformed("key file is not a 64-character hex private key");
    let body = text.get(..64).ok_or(KeystoreError::Malformed(
        "key file is not a 64-character hex private key",
    ))?;
    if !text
        .get(64..)
        .unwrap_or(&[])
        .iter()
        .all(u8::is_ascii_whitespace)
    {
        return Err(bad);
    }
    let raw =
        Zeroizing::new(hex::decode(body).map_err(|_| {
            KeystoreError::Malformed("key file is not a 64-character hex private key")
        })?);
    let key = KeyBytes::from_slice(&raw).ok_or(bad)?;
    let s = LocalSigner::from_key_bytes(&key)?;
    Ok((key, s.address()))
}

fn uuid_v4() -> Result<String, KeystoreError> {
    let mut b: [u8; 16] = random()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// Keystore v3 JSON of `key` under `password` (geth `EncryptKey`, AES-128-CTR, keccak MAC).
pub fn encrypt(key: &KeyBytes, password: &Password, kdf: Kdf) -> Result<String, KeystoreError> {
    let address = LocalSigner::from_key_bytes(key)?.address();
    let (log_n, r, p) = kdf.params();
    let salt: [u8; 32] = random()?;
    let iv: [u8; 16] = random()?;
    let params = scrypt::Params::new(log_n, r, p)
        .map_err(|_| KeystoreError::Malformed("scrypt parameters out of range"))?;
    let mut dk = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(password.as_bytes(), &salt, &params, dk.as_mut_slice())
        .map_err(|_| KeystoreError::Malformed("scrypt output length"))?;
    let mut ct = key.as_bytes().to_vec();
    let mut cipher = ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&dk[..16], &iv)
        .map_err(|_| KeystoreError::Malformed("iv length"))?;
    cipher.apply_keystream(&mut ct);
    let mac = keccak256([&dk[16..32], &ct[..]].concat());
    Ok(json!({
        "address": hex::encode(address),
        "crypto": {
            "cipher": "aes-128-ctr",
            "ciphertext": hex::encode(&ct),
            "cipherparams": {"iv": hex::encode(iv)},
            "kdf": "scrypt",
            "kdfparams": {"dklen": 32, "n": 1u64 << log_n, "p": p, "r": r, "salt": hex::encode(salt)},
            "mac": hex::encode(mac),
        },
        "id": uuid_v4()?,
        "version": 3,
    })
    .to_string())
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// geth `keyFileName`: `UTC--2006-01-02T15-04-05.000000000Z--<address hex>`.
pub fn file_name(address: Address, unix_secs: u64, nanos: u32) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let rem = unix_secs % 86_400;
    let (y, mo, d) = civil(days);
    format!(
        "UTC--{y:04}-{mo:02}-{d:02}T{:02}-{:02}-{:02}.{nanos:09}Z--{}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        hex::encode(address)
    )
}

fn now() -> (u64, u32) {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()))
}

fn ensure_dir(dir: &Path) -> Result<(), KeystoreError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(io_err(dir))
}

/// Writes `text` to `path` with mode 0600; the file must not exist.
fn write_new(path: &Path, text: &str) -> Result<(), KeystoreError> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io_err(path))?;
    f.write_all(text.as_bytes()).map_err(io_err(path))?;
    f.sync_all().map_err(io_err(path))
}

/// One account of the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub address: Address,
    pub path: PathBuf,
}

/// The accounts of `dir`, sorted by path (geth `accountCache`): files whose JSON has a valid
/// `address`; nothing is decrypted. Dot files, editor backups (`~`) and directories are skipped.
/// A missing directory has no accounts.
pub fn list(dir: &Path) -> Result<Vec<Account>, KeystoreError> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(io_err(dir)(e)),
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with('~') || !e.file_type().is_ok_and(|t| t.is_file())
        {
            continue;
        }
        if e.metadata().map_or(true, |m| m.len() > 64 * 1024) {
            continue;
        }
        let Ok(text) = fs::read(&path) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(a) = v.get("address").and_then(|a| a.as_str()) else {
            continue;
        };
        let a = a.strip_prefix("0x").unwrap_or(a);
        if let Ok(b) = hex::decode(a) {
            if b.len() == 20 {
                out.push(Account {
                    address: Address::from_slice(&b),
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Stores `key` in `dir` as a new file; refuses an address the directory already holds (geth
/// `ErrAccountAlreadyExists`).
pub fn store(
    dir: &Path,
    key: &KeyBytes,
    password: &Password,
    kdf: Kdf,
) -> Result<Account, KeystoreError> {
    let address = LocalSigner::from_key_bytes(key)?.address();
    if list(dir)?.iter().any(|a| a.address == address) {
        return Err(KeystoreError::AccountExists(address));
    }
    ensure_dir(dir)?;
    let text = encrypt(key, password, kdf)?;
    let (s, n) = now();
    let path = dir.join(file_name(address, s, n));
    write_new(&path, &text)?;
    Ok(Account { address, path })
}

/// `account new`.
pub fn create(dir: &Path, password: &Password, kdf: Kdf) -> Result<Account, KeystoreError> {
    let (key, _) = generate()?;
    store(dir, &key, password, kdf)
}

/// `account import <keyfile>`: a hex private key file (mode 0600 unless `allow`).
pub fn import(
    dir: &Path,
    keyfile: &Path,
    password: &Password,
    kdf: Kdf,
    allow: Option<crate::AllowInsecurePermissions>,
) -> Result<Account, KeystoreError> {
    let text = Zeroizing::new(crate::secure_file::read_secret_file(
        keyfile,
        allow,
        MAX_KEYFILE,
    )?);
    let (key, _) = parse_hex_key(&text)?;
    store(dir, &key, password, kdf)
}

/// `account update <address>`: decrypts with `old`, writes the same key under `new` in place
/// (a temporary file renamed over the old one, geth `writeKeyFile`).
pub fn update(
    dir: &Path,
    address: Address,
    old: &Password,
    new: &Password,
    kdf: Kdf,
) -> Result<Account, KeystoreError> {
    let acct = list(dir)?
        .into_iter()
        .find(|a| a.address == address)
        .ok_or(KeystoreError::NoSuchAccount(address))?;
    let text = crate::secure_file::read_secret_file(&acct.path, None, 64 * 1024)?;
    let (key, _) = decrypt_key(&text, old)?;
    let json = encrypt(&key, new, kdf)?;
    let tmp = acct.path.with_extension("tmp-update");
    let _ = fs::remove_file(&tmp);
    write_new(&tmp, &json)?;
    fs::rename(&tmp, &acct.path).map_err(io_err(&acct.path))?;
    Ok(acct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geth_file_names() {
        let a = Address::repeat_byte(0xab);
        // 2017-05-02T08:24:31.000000123Z
        assert_eq!(
            file_name(a, 1_493_713_471, 123),
            format!("UTC--2017-05-02T08-24-31.000000123Z--{}", "ab".repeat(20))
        );
        assert_eq!(
            file_name(a, 0, 0),
            format!("UTC--1970-01-01T00-00-00.000000000Z--{}", "ab".repeat(20))
        );
        // a leap day
        assert!(file_name(a, 951_782_400, 0).starts_with("UTC--2000-02-29T00-00-00"));
    }

    #[test]
    fn hex_key_files() {
        let k = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let (_, a) = parse_hex_key(format!("{k}\n").as_bytes()).ok().unzip();
        assert_eq!(
            a.map(|a| format!("{a:#x}")),
            Some("0x2c7536e3605d9c16a7a3d7b1898e529396a65c23".into())
        );
        assert!(parse_hex_key(&k.as_bytes()[..63]).is_err());
        assert!(parse_hex_key(format!("{k}x").as_bytes()).is_err());
        assert!(parse_hex_key("zz".repeat(32).as_bytes()).is_err());
        assert!(parse_hex_key("00".repeat(32).as_bytes()).is_err());
    }

    #[test]
    fn uuid_is_version_4() {
        let u = uuid_v4().unwrap_or_else(|_| String::new());
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
    }
}
