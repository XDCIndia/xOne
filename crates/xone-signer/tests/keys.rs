//! Typed errors, no partial state, permission rule, node key file, remote fencing.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests: failure is a panic

mod common;

use alloy_primitives::{hex, keccak256};
use common::{keystore_json, random_key, write_file};
use xone_primitives::{Domain, Signer};
use xone_signer::local_keystore::{decrypt, load};
use xone_signer::nodekey::{self, NodeKeyError};
use xone_signer::remote::{FencingToken, InProcessSigner, Refusal, RemoteSigner, SignRequest};
use xone_signer::{AllowInsecurePermissions, KeystoreError, LocalSigner, Password};

fn pw(s: &str) -> Password {
    Password::new(s.as_bytes().to_vec())
}

#[test]
fn failed_unlock_leaves_no_state_and_the_next_unlock_works() {
    let json = keystore_json(&random_key(), "right");
    for _ in 0..3 {
        assert_eq!(
            decrypt(json.as_bytes(), &pw("wrong")).unwrap_err(),
            KeystoreError::WrongPassword
        );
    }
    // `decrypt` returns the only handle to a key; on Err there is none. The same file still unlocks.
    let s: LocalSigner = decrypt(json.as_bytes(), &pw("right")).unwrap();
    assert!(s.sign(Domain::VoteV2, &keccak256(b"x")).is_ok());
}

#[test]
fn malformed_and_unsupported_inputs_are_typed() {
    let json = keystore_json(&random_key(), "p");
    let p = pw("p");
    let m = |s: &str| decrypt(s.as_bytes(), &p).unwrap_err();
    assert!(matches!(m("{"), KeystoreError::Malformed(_)));
    assert!(matches!(
        m(&json.replace(r#""version":3"#, r#""version":1"#)),
        KeystoreError::Malformed(_)
    ));
    assert!(matches!(
        m(&json.replace(r#""kdf":"pbkdf2""#, r#""kdf":"argon2""#)),
        KeystoreError::UnsupportedKdf(_)
    ));
    assert!(matches!(
        m(&json.replace("hmac-sha256", "hmac-sha1")),
        KeystoreError::UnsupportedKdf(_)
    ));
    assert!(matches!(
        m(&json.replace("aes-128-ctr", "aes-256-cbc")),
        KeystoreError::UnsupportedCipher(_)
    ));
    assert!(matches!(
        m(&json.replace(r#""c":1000"#, r#""c":4000000000"#)),
        KeystoreError::ResourcePolicy(_)
    ));
    // Flip one ciphertext nibble: the MAC fails, reported as a wrong password.
    let ct = json.split(r#""ciphertext":""#).nth(1).unwrap();
    let flipped = if ct.starts_with('0') { "1" } else { "0" };
    let bad = json.replacen(
        &format!(r#""ciphertext":"{}"#, &ct[..1]),
        &format!(r#""ciphertext":"{flipped}"#),
        1,
    );
    assert_ne!(bad, json);
    assert_eq!(m(&bad), KeystoreError::WrongPassword);
    // A hostile scrypt cost is refused before any memory is allocated.
    let scrypt = json.replace(
        r#""kdf":"pbkdf2","kdfparams":{"c":1000,"#,
        r#""kdf":"scrypt","kdfparams":{"n":1073741824,"r":8,"p":1,"#,
    );
    assert!(matches!(m(&scrypt), KeystoreError::ResourcePolicy(_)));
}

#[test]
fn wide_mode_refuses_unless_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let (ks, pwf) = (dir.path().join("ks.json"), dir.path().join("pw"));
    let key = random_key();
    write_file(&ks, keystore_json(&key, "secret").as_bytes(), 0o600);
    write_file(&pwf, b"secret\r\nsecond line\n", 0o600);
    let ok = load(&ks, &pwf, None).unwrap();

    for (bad_ks, bad_pw) in [
        (0o640, 0o600),
        (0o600, 0o604),
        (0o700, 0o600),
        (0o666, 0o666),
    ] {
        write_file(&ks, std::fs::read(&ks).unwrap().as_slice(), bad_ks);
        write_file(&pwf, std::fs::read(&pwf).unwrap().as_slice(), bad_pw);
        let err = load(&ks, &pwf, None).unwrap_err();
        assert!(
            matches!(err, KeystoreError::Permissions { .. }),
            "{bad_ks:o} {bad_pw:o}: {err:?}"
        );
        // The explicit override loads it.
        let s = load(&ks, &pwf, Some(AllowInsecurePermissions)).unwrap();
        assert_eq!(s.address(), ok.address());
    }
    // 0400 is narrower than 0600 and fine.
    write_file(&ks, std::fs::read(&ks).unwrap().as_slice(), 0o400);
    write_file(&pwf, std::fs::read(&pwf).unwrap().as_slice(), 0o400);
    assert!(load(&ks, &pwf, None).is_ok());
}

#[test]
fn missing_file_and_directory_are_typed_io_or_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let e = load(&dir.path().join("none"), &dir.path().join("none"), None).unwrap_err();
    assert!(matches!(e, KeystoreError::Io { .. }));
    let pwf = dir.path().join("pw");
    write_file(&pwf, b"x", 0o600);
    assert!(matches!(
        load(dir.path(), &pwf, None).unwrap_err(),
        KeystoreError::Malformed(_)
    ));
}

#[test]
fn nodekey_create_then_load_is_stable_and_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let datadir = dir.path().join("xone");
    let a = nodekey::load_or_create(&datadir, None).unwrap();
    let file = datadir.join("nodekey");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&datadir).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    let text = std::fs::read(&file).unwrap();
    assert_eq!(text.len(), 64);
    assert!(hex::decode(&text).is_ok());
    let b = nodekey::load_or_create(&datadir, None).unwrap();
    assert_eq!(a.node_id(), b.node_id());
    assert_eq!(
        std::fs::read_dir(&datadir).unwrap().count(),
        1,
        "no temp file left"
    );
}

#[test]
fn nodekey_formats_and_errors_follow_geth() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("nodekey");
    let key = hex::encode(random_key());
    let load = |content: &str, mode| {
        write_file(&f, content.as_bytes(), mode);
        nodekey::load(&f, None)
    };
    assert!(load(&key, 0o600).is_ok());
    assert!(load(&format!("{key}\n"), 0o600).is_ok());
    assert!(load(&format!("{key}\r\n"), 0o600).is_ok());
    assert!(matches!(
        load(&format!("{key}\n\n\n"), 0o600),
        Err(NodeKeyError::Malformed(_))
    ));
    assert!(matches!(
        load(&key[..62], 0o600),
        Err(NodeKeyError::Malformed(_))
    ));
    assert!(matches!(
        load(&format!("{key}ab"), 0o600),
        Err(NodeKeyError::Malformed(_))
    ));
    assert!(matches!(
        load(&"zz".repeat(32), 0o600),
        Err(NodeKeyError::Malformed(_))
    ));
    assert!(matches!(
        load(&"00".repeat(32), 0o600),
        Err(NodeKeyError::Malformed(_))
    ));
    assert!(matches!(
        load(&key, 0o644),
        Err(NodeKeyError::Permissions { .. })
    ));
    assert!(nodekey::load(&f, Some(AllowInsecurePermissions)).is_ok());
    // A corrupt file is never replaced by a fresh identity.
    write_file(&f, b"garbage", 0o600);
    assert!(matches!(
        nodekey::load_or_create(dir.path(), None),
        Err(NodeKeyError::Malformed(_))
    ));
    assert_eq!(std::fs::read(&f).unwrap(), b"garbage");
}

#[test]
fn remote_refuses_a_stale_fencing_token() {
    let json = keystore_json(&random_key(), "p");
    let remote = InProcessSigner::new(decrypt(json.as_bytes(), &pw("p")).unwrap());
    let req = |n| SignRequest {
        domain: Domain::VoteV2,
        digest: keccak256(b"d"),
        fence: FencingToken(n),
    };
    let first = remote.sign(&req(5)).unwrap();
    assert_eq!(
        remote.sign(&req(5)).unwrap(),
        first,
        "same token, deterministic signature"
    );
    assert!(remote.sign(&req(6)).is_ok(), "a newer holder takes over");
    assert_eq!(
        remote.sign(&req(5)).unwrap_err(),
        Refusal::StaleFence {
            presented: FencingToken(5),
            current: FencingToken(6)
        }
    );
}

/// Hostile keystore-v3 files (#61): each is refused with its typed error, quickly (no KDF runs for the
/// shape and cost refusals) and without a signer.
#[test]
fn hostile_keystores_are_refused_before_any_derivation() {
    let json = keystore_json(&random_key(), "p");
    let p = pw("p");
    let t = std::time::Instant::now();
    let m = |s: &str| decrypt(s.as_bytes(), &p).unwrap_err();
    // oversized file
    let big = format!("{json}{}", " ".repeat(70_000));
    assert!(matches!(m(&big), KeystoreError::ResourcePolicy(_)));
    // absurd scrypt N, and a cost that overflows u64
    let scrypt = |kp: &str| {
        json.replace(
            r#""kdf":"pbkdf2","kdfparams":{"c":1000,"#,
            &format!(r#""kdf":"scrypt","kdfparams":{{{kp},"#),
        )
    };
    assert!(matches!(
        m(&scrypt(r#""n":1099511627776,"r":8,"p":1"#)),
        KeystoreError::ResourcePolicy(_)
    ));
    assert!(matches!(
        m(&scrypt(
            r#""n":4611686018427387904,"r":4611686018427387904,"p":1"#
        )),
        KeystoreError::ResourcePolicy(_)
    ));
    assert!(matches!(
        m(&scrypt(r#""n":3,"r":8,"p":1"#)),
        KeystoreError::Malformed(_)
    ));
    // negative and fractional numbers
    assert!(matches!(
        m(&scrypt(r#""n":-262144,"r":8,"p":1"#)),
        KeystoreError::Malformed(_)
    ));
    assert!(matches!(
        m(&scrypt(r#""n":262144.5,"r":8,"p":1"#)),
        KeystoreError::Malformed(_)
    ));
    // unknown cipher
    assert!(matches!(
        m(&json.replace("aes-128-ctr", "aes-256-gcm")),
        KeystoreError::UnsupportedCipher(_)
    ));
    // IV, ciphertext and MAC lengths, before the KDF
    let field = |name: &str| {
        let tail = json.split(&format!(r#""{name}":""#)).nth(1).unwrap();
        tail[..tail.find('"').unwrap()].to_owned()
    };
    for (name, cut) in [("iv", 2), ("ciphertext", 2), ("mac", 2)] {
        let v = field(name);
        let shorter = &v[..v.len() - cut];
        let longer = format!("{v}00");
        for bad in [shorter.to_owned(), longer] {
            let j = json.replacen(
                &format!(r#""{name}":"{v}""#),
                &format!(r#""{name}":"{bad}""#),
                1,
            );
            assert!(matches!(m(&j), KeystoreError::Malformed(_)), "{name}");
        }
        let empty = json.replacen(&format!(r#""{name}":"{v}""#), &format!(r#""{name}":"""#), 1);
        assert!(
            matches!(m(&empty), KeystoreError::Malformed(_)),
            "{name} empty"
        );
    }
    // duplicate required field
    assert!(matches!(
        m(&json.replacen(r#""version":3"#, r#""version":3,"version":3"#, 1)),
        KeystoreError::Malformed(_)
    ));
    // a wrong MAC, and a MAC of the right length for another key
    let mac = field("mac");
    let other = format!(
        "{}{}",
        if mac.starts_with('0') { "1" } else { "0" },
        &mac[1..]
    );
    assert_eq!(
        m(&json.replacen(&mac, &other, 1)),
        KeystoreError::WrongPassword
    );
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );
}
