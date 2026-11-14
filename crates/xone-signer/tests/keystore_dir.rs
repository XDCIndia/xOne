//! `xone account` storage (#182): files written here decrypt with the geth-compatible reader, a
//! geth-written file is re-encrypted by `update`, files are 0600 in a 0700 directory with geth
//! names, and no secret reaches a written file in the clear.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests: failure is a panic

use std::os::unix::fs::PermissionsExt;

use alloy_primitives::Address;
use xone_signer::keystore_dir::{self, Kdf};
use xone_signer::local_keystore::decrypt;
use xone_signer::{KeystoreError, Password};

const KEY: &str = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

fn pw(s: &str) -> Password {
    Password::new(s.as_bytes().to_vec())
}

fn mode(p: &std::path::Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn new_import_list_and_update() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("keystore");
    assert!(keystore_dir::list(&dir).unwrap().is_empty());

    let a = keystore_dir::create(&dir, &pw("one"), Kdf::Light).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&a.path), 0o600);
    let name = a.path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("UTC--") && name.ends_with(&alloy_primitives::hex::encode(a.address)));
    let text = std::fs::read(&a.path).unwrap();
    assert_eq!(decrypt(&text, &pw("one")).unwrap().address(), a.address);
    assert_eq!(
        decrypt(&text, &pw("two")).err(),
        Some(KeystoreError::WrongPassword)
    );

    // import a hex key file; the key is not in the keystore file in the clear
    let kf = tmp.path().join("key.hex");
    std::fs::write(&kf, format!("{KEY}\n")).unwrap();
    std::fs::set_permissions(&kf, std::fs::Permissions::from_mode(0o600)).unwrap();
    let b = keystore_dir::import(&dir, &kf, &pw("k"), Kdf::Light, None).unwrap();
    let want: Address = "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23"
        .parse()
        .unwrap();
    assert_eq!(b.address, want);
    let stored = std::fs::read_to_string(&b.path).unwrap();
    assert!(!stored.contains(KEY));
    // the same key twice is refused
    let again = keystore_dir::import(&dir, &kf, &pw("k"), Kdf::Light, None);
    assert_eq!(again.err(), Some(KeystoreError::AccountExists(want)));

    let all = keystore_dir::list(&dir).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.windows(2).all(|w| w[0].path < w[1].path));

    // update: the old password no longer opens it, the new one does, the path is the same
    let u = keystore_dir::update(&dir, want, &pw("k"), &pw("k2"), Kdf::Light).unwrap();
    assert_eq!(u.path, b.path);
    assert_eq!(mode(&u.path), 0o600);
    let text = std::fs::read(&u.path).unwrap();
    assert_eq!(decrypt(&text, &pw("k2")).unwrap().address(), want);
    assert!(decrypt(&text, &pw("k")).is_err());
    assert!(keystore_dir::update(&dir, want, &pw("wrong"), &pw("x"), Kdf::Light).is_err());
    assert_eq!(keystore_dir::list(&dir).unwrap().len(), 2);
}

#[test]
fn a_geth_written_file_round_trips_through_update() {
    #[derive(serde::Deserialize)]
    struct Vector {
        password: String,
        address: String,
        keystore: serde_json::Value,
    }
    let v: Vector = serde_json::from_slice(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/keystore/scrypt-light.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_owned();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let f = dir.join("UTC--2020-01-01T00-00-00.000000000Z--geth");
    std::fs::write(&f, v.keystore.to_string()).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    let addr: Address = v.address.parse().unwrap();
    assert_eq!(keystore_dir::list(&dir).unwrap()[0].address, addr);
    keystore_dir::update(&dir, addr, &pw(&v.password), &pw("new"), Kdf::Light).unwrap();
    let text = std::fs::read(&f).unwrap();
    assert_eq!(decrypt(&text, &pw("new")).unwrap().address(), addr);
    // the written JSON has geth's shape
    let j: serde_json::Value = serde_json::from_slice(&text).unwrap();
    assert_eq!(j["version"], 3);
    assert_eq!(j["crypto"]["kdf"], "scrypt");
    assert_eq!(j["crypto"]["cipher"], "aes-128-ctr");
    assert_eq!(j["crypto"]["kdfparams"]["n"], 4096);
    assert_eq!(j["crypto"]["kdfparams"]["p"], 6);
    assert_eq!(j["crypto"]["kdfparams"]["r"], 8);
    assert_eq!(j["crypto"]["kdfparams"]["dklen"], 32);
}
