//! geth-generated keystore files (see tests/vectors/README.md): decrypt, match the address,
//! sign and recover. Wrong password is a typed error.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests: failure is a panic

use alloy_primitives::{hex, keccak256, Address};
use serde::Deserialize;
use xone_primitives::{Domain, Secp256k1Verifier, Signer, Verifier};
use xone_signer::local_keystore::decrypt;
use xone_signer::{KeystoreError, Password};

#[derive(Deserialize)]
struct Vector {
    name: String,
    password: String,
    address: String,
    keystore: serde_json::Value,
}

fn vectors() -> Vec<Vector> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/keystore");
    let mut v: Vec<Vector> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

#[test]
fn geth_vectors_decrypt_to_the_expected_address() {
    let all = vectors();
    let names: Vec<_> = all.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "pbkdf2",
            "scrypt-light",
            "scrypt-light-xdc-address",
            "scrypt-standard"
        ]
    );
    for v in &all {
        let json = serde_json::to_vec(&v.keystore).unwrap();
        let signer = decrypt(&json, &Password::new(v.password.clone().into_bytes())).unwrap();
        let want = Address::from_slice(&hex::decode(&v.address).unwrap());
        assert_eq!(signer.address(), want, "{}", v.name);
        let digest = keccak256(v.name.as_bytes());
        let sig = signer.sign(Domain::SealV2, &digest).unwrap();
        assert_eq!(
            Secp256k1Verifier
                .recover(Domain::SealV2, &digest, &sig)
                .unwrap(),
            want
        );
    }
}

#[test]
fn xdc_prefixed_address_field_is_accepted_and_checked() {
    let all = vectors();
    let v = all
        .iter()
        .find(|v| v.name == "scrypt-light-xdc-address")
        .unwrap();
    let a = v.keystore["address"].as_str().unwrap();
    assert!(a.starts_with("xdc"));
    // A different address in the same field is refused.
    let mut k = v.keystore.clone();
    k["address"] = format!("xdc{}", "11".repeat(20)).into();
    let pw = Password::new(v.password.clone().into_bytes());
    assert_eq!(
        decrypt(&serde_json::to_vec(&k).unwrap(), &pw).unwrap_err(),
        KeystoreError::Malformed("address field does not match the key")
    );
}

#[test]
fn wrong_password_is_typed_for_every_vector_kdf() {
    for v in vectors().iter().filter(|v| v.name != "scrypt-standard") {
        let json = serde_json::to_vec(&v.keystore).unwrap();
        let err = decrypt(&json, &Password::new(b"not the password".to_vec())).unwrap_err();
        assert_eq!(err, KeystoreError::WrongPassword, "{}", v.name);
    }
}
