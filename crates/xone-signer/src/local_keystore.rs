//! Local backend: geth keystore v3 (`spec/keys/keystore-v3.md`) and the [`LocalSigner`].

use std::io;
use std::path::{Path, PathBuf};

use alloy_primitives::{hex, keccak256, Address, B256, B512};
use ctr::cipher::{KeyIvInit, StreamCipher};
use k256::ecdsa::SigningKey;
use serde::Deserialize;
use sha2::Sha256;
use thiserror::Error;
use xone_primitives::{AlgorithmId, CryptoError, Domain, Signature, Signer};
use zeroize::Zeroizing;

use crate::secret::{KeyBytes, Password};
use crate::secure_file::{read_secret_file, AllowInsecurePermissions, FileError};

const MAX_FILE: u64 = 64 * 1024;
/// scrypt memory is 128 * r * N bytes; refuse a file that asks for more than 1 GiB
/// (geth standard is 256 MiB). geth has no cap.
const MAX_SCRYPT_BYTES: u64 = 1 << 30;
const MAX_PBKDF2_C: u64 = 1 << 24;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeystoreError {
    #[error("wrong password (MAC mismatch)")]
    WrongPassword,
    #[error("malformed keystore: {0}")]
    Malformed(&'static str),
    #[error("unsupported kdf {0:?}")]
    UnsupportedKdf(String),
    #[error("unsupported cipher {0:?}")]
    UnsupportedCipher(String),
    #[error("{}: mode {mode:o} is wider than 0600", .path.display())]
    Permissions { path: PathBuf, mode: u32 },
    #[error("{}: i/o error: {kind}", .path.display())]
    Io { path: PathBuf, kind: io::ErrorKind },
}

impl From<FileError> for KeystoreError {
    fn from(e: FileError) -> Self {
        match e {
            FileError::Permissions { path, mode } => Self::Permissions { path, mode },
            FileError::Io { path, kind } => Self::Io { path, kind },
            FileError::Unusable { .. } => Self::Malformed("not a regular file, or too large"),
        }
    }
}

#[derive(Deserialize)]
struct KeystoreJson {
    #[serde(default)]
    address: String,
    #[serde(alias = "Crypto")]
    crypto: CryptoJson,
    version: u64,
}

#[derive(Deserialize)]
struct CryptoJson {
    cipher: String,
    ciphertext: String,
    cipherparams: CipherParams,
    kdf: String,
    kdfparams: KdfParams,
    mac: String,
}

#[derive(Deserialize)]
struct CipherParams {
    iv: String,
}

#[derive(Deserialize)]
struct KdfParams {
    dklen: Option<u64>,
    salt: Option<String>,
    n: Option<u64>,
    r: Option<u64>,
    p: Option<u64>,
    c: Option<u64>,
    prf: Option<String>,
}

/// The validator or node key behind [`xone_primitives::Signer`]. Prints its address only.
pub struct LocalSigner {
    key: SigningKey,
    address: Address,
}

impl std::fmt::Debug for LocalSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSigner")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl LocalSigner {
    pub(crate) fn from_key_bytes(k: &KeyBytes) -> Result<Self, KeystoreError> {
        let key = SigningKey::from_slice(k.as_bytes()).map_err(|_| {
            KeystoreError::Malformed("private key is zero or not below the curve order")
        })?;
        let point = key.verifying_key().to_encoded_point(false);
        let address = Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..]);
        Ok(Self { key, address })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// The 64-byte uncompressed public key without the 0x04 prefix (the devp2p node id form).
    pub fn public_key(&self) -> B512 {
        let point = self.key.verifying_key().to_encoded_point(false);
        B512::from_slice(&point.as_bytes()[1..])
    }
}

impl Signer for LocalSigner {
    fn algorithm(&self) -> AlgorithmId {
        AlgorithmId::Secp256k1Keccak
    }

    /// RFC 6979 deterministic, low-S, `r || s || v` (the geth `crypto.Sign` layout).
    fn sign(&self, domain: Domain, digest: &B256) -> Result<Signature, CryptoError> {
        tracing::trace!(?domain, %digest, "local sign");
        let (sig, id) = self
            .key
            .sign_prehash_recoverable(digest.as_slice())
            .map_err(|_| CryptoError::Sign)?;
        let mut bytes = sig.to_bytes().to_vec();
        bytes.push(id.to_byte());
        Ok(Signature {
            algorithm: self.algorithm(),
            bytes: bytes.into(),
        })
    }
}

/// First line of the file, trailing `\r` removed (geth `readPasswordFromFile`).
pub fn read_password_file(
    path: &Path,
    allow: Option<AllowInsecurePermissions>,
) -> Result<Password, KeystoreError> {
    let raw = read_secret_file(path, allow, MAX_FILE)?;
    let line = raw.split(|b| *b == b'\n').next().unwrap_or(&[]);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    Ok(Password::new(line.to_vec()))
}

/// Unlock a keystore file with a password file. Both files must be mode 0600 unless
/// `allow` is given.
pub fn load(
    keystore: &Path,
    password_file: &Path,
    allow: Option<AllowInsecurePermissions>,
) -> Result<LocalSigner, KeystoreError> {
    let password = read_password_file(password_file, allow)?;
    let json = read_secret_file(keystore, allow, MAX_FILE)?;
    let signer = decrypt(&json, &password)?;
    tracing::info!(address = %signer.address(), "keystore unlocked");
    Ok(signer)
}

/// Decrypt keystore v3 JSON. Pure: a failure returns an error and no signer, and every
/// intermediate secret is a zeroizing buffer, so there is no partial unlock state.
pub fn decrypt(json: &[u8], password: &Password) -> Result<LocalSigner, KeystoreError> {
    let r = decrypt_inner(json, password);
    if let Err(e) = &r {
        tracing::debug!(error = %e, "keystore unlock failed");
    }
    r
}

fn hex_field(s: &str, what: &'static str) -> Result<Vec<u8>, KeystoreError> {
    hex::decode(s).map_err(|_| KeystoreError::Malformed(what))
}

fn decrypt_inner(json: &[u8], password: &Password) -> Result<LocalSigner, KeystoreError> {
    let ks: KeystoreJson =
        serde_json::from_slice(json).map_err(|_| KeystoreError::Malformed("not keystore json"))?;
    if ks.version != 3 {
        return Err(KeystoreError::Malformed("version is not 3"));
    }
    let c = &ks.crypto;
    if c.cipher != "aes-128-ctr" {
        return Err(KeystoreError::UnsupportedCipher(c.cipher.clone()));
    }
    let kp = &c.kdfparams;
    let salt = hex_field(kp.salt.as_deref().unwrap_or(""), "kdf salt")?;
    let dklen = kp.dklen.ok_or(KeystoreError::Malformed("dklen missing"))?;
    if !(32..=128).contains(&dklen) {
        return Err(KeystoreError::Malformed("dklen out of range"));
    }
    let dklen = usize::try_from(dklen).map_err(|_| KeystoreError::Malformed("dklen"))?;
    let need = |v: Option<u64>| v.ok_or(KeystoreError::Malformed("kdf parameter missing"));
    let mut dk = Zeroizing::new(vec![0u8; dklen]);
    match c.kdf.as_str() {
        "scrypt" => {
            let (n, r, p) = (need(kp.n)?, need(kp.r)?, need(kp.p)?);
            let bytes = n.checked_mul(r).and_then(|x| x.checked_mul(128));
            if !n.is_power_of_two()
                || n < 2
                || r == 0
                || p == 0
                || p > 64
                || bytes.is_none_or(|b| b > MAX_SCRYPT_BYTES)
            {
                return Err(KeystoreError::Malformed("scrypt parameters out of range"));
            }
            let conv = |v: u64| {
                u32::try_from(v).map_err(|_| KeystoreError::Malformed("scrypt parameters"))
            };
            let params = scrypt::Params::new(
                u8::try_from(n.trailing_zeros())
                    .map_err(|_| KeystoreError::Malformed("scrypt n"))?,
                conv(r)?,
                conv(p)?,
            )
            .map_err(|_| KeystoreError::Malformed("scrypt parameters out of range"))?;
            scrypt::scrypt(password.as_bytes(), &salt, &params, &mut dk)
                .map_err(|_| KeystoreError::Malformed("scrypt output length"))?;
        }
        "pbkdf2" => {
            if kp.prf.as_deref() != Some("hmac-sha256") {
                return Err(KeystoreError::UnsupportedKdf(
                    "pbkdf2 with a prf other than hmac-sha256".into(),
                ));
            }
            let iters = need(kp.c)?;
            if iters == 0 || iters > MAX_PBKDF2_C {
                return Err(KeystoreError::Malformed("pbkdf2 c out of range"));
            }
            let iters = u32::try_from(iters).map_err(|_| KeystoreError::Malformed("pbkdf2 c"))?;
            pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iters, &mut dk);
        }
        other => {
            return Err(KeystoreError::UnsupportedKdf(
                other.chars().take(32).collect(),
            ))
        }
    }
    let ct = hex_field(&c.ciphertext, "ciphertext")?;
    let iv = hex_field(&c.cipherparams.iv, "iv")?;
    let mac = hex_field(&c.mac, "mac")?;
    // MAC = keccak256(dk[16..32] || ciphertext), checked before any decryption.
    if keccak256([&dk[16..32], &ct[..]].concat()).as_slice() != mac.as_slice() {
        return Err(KeystoreError::WrongPassword);
    }
    let mut plain = Zeroizing::new(ct);
    let mut cipher = ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&dk[..16], &iv)
        .map_err(|_| KeystoreError::Malformed("iv length"))?;
    cipher.apply_keystream(&mut plain);
    let key =
        KeyBytes::from_slice(&plain).ok_or(KeystoreError::Malformed("key is not 32 bytes"))?;
    let signer = LocalSigner::from_key_bytes(&key)?;
    if !ks.address.is_empty() {
        let a = ks.address.as_str();
        let a = ["0x", "0X", "xdc", "XDC", "Xdc"]
            .iter()
            .find_map(|p| a.strip_prefix(p))
            .unwrap_or(a);
        let want = hex::decode(a).map_err(|_| KeystoreError::Malformed("address field"))?;
        if want.as_slice() != signer.address.as_slice() {
            return Err(KeystoreError::Malformed(
                "address field does not match the key",
            ));
        }
    }
    Ok(signer)
}
