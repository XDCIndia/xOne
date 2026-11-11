#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Test helpers. Every key here is random and throwaway.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use aes::Aes128;
use alloy_primitives::{hex, keccak256};
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::Sha256;

pub fn random_key() -> [u8; 32] {
    loop {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).unwrap();
        if k::ecdsa::SigningKey::from_slice(&k).is_ok() {
            return k;
        }
    }
}

use k256 as k;

/// Keystore v3 JSON with the pbkdf2 KDF (low iteration count, for speed) and no address field.
pub fn keystore_json(key: &[u8; 32], password: &str) -> String {
    let (mut salt, mut iv) = ([0u8; 32], [0u8; 16]);
    getrandom::fill(&mut salt).unwrap();
    getrandom::fill(&mut iv).unwrap();
    let mut dk = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 1000, &mut dk);
    let mut ct = *key;
    ctr::Ctr128BE::<Aes128>::new_from_slices(&dk[..16], &iv)
        .unwrap()
        .apply_keystream(&mut ct);
    let mac = keccak256([&dk[16..], &ct[..]].concat());
    format!(
        r#"{{"version":3,"id":"00000000-0000-4000-8000-000000000000","crypto":{{"cipher":"aes-128-ctr","ciphertext":"{}","cipherparams":{{"iv":"{}"}},"kdf":"pbkdf2","kdfparams":{{"c":1000,"dklen":32,"prf":"hmac-sha256","salt":"{}"}},"mac":"{}"}}}}"#,
        hex::encode(ct),
        hex::encode(iv),
        hex::encode(salt),
        hex::encode(mac)
    )
}

pub fn write_file(path: &Path, content: &[u8], mode: u32) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
