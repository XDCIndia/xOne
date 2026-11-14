#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! #61: the hostile keystore-v3 matrix of the issue's acceptance rows, in its own test binary so the
//! process memory high-water mark measures this intake only. Every key and password is generated.
//! The fuzz target over the same boundary is `fuzz/fuzz_targets/keystore_decrypt.rs`.

mod common;

use common::{keystore_json, random_key};
use std::time::{Duration, Instant};
use xone_signer::{local_keystore::decrypt, KeystoreError, Password};

const PASSWORD: &str = "intake-matrix-password";
/// secp256k1 group order n.
const N: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141";

/// Linux `VmHWM` (peak resident set) in KiB.
fn peak_rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap()
}

fn key_of(hex32: &str) -> [u8; 32] {
    let mut k = [0u8; 32];
    k.copy_from_slice(&alloy_primitives::hex::decode(hex32).unwrap());
    k
}

/// `kdfparams` replaced whole, so each case states every parameter.
fn with_kdf(json: &str, kdf: &str, params: &str) -> String {
    let start = json.find(r#""kdf":"pbkdf2""#).unwrap();
    let end = start + json[start..].find('}').unwrap() + 1;
    format!(
        r#"{}"kdf":"{kdf}","kdfparams":{{{params}}}{}"#,
        &json[..start],
        &json[end..]
    )
}

fn field(json: &str, name: &str) -> String {
    let tail = json.split(&format!(r#""{name}":""#)).nth(1).unwrap();
    tail[..tail.find('"').unwrap()].to_owned()
}

fn set_field(json: &str, name: &str, v: &str) -> String {
    let old = field(json, name);
    json.replacen(
        &format!(r#""{name}":"{old}""#),
        &format!(r#""{name}":"{v}""#),
        1,
    )
}

#[derive(Debug, PartialEq)]
enum Want {
    Malformed,
    Kdf,
    Cipher,
    Policy,
    WrongPassword,
}

fn class(e: &KeystoreError) -> Want {
    match e {
        KeystoreError::Malformed(_) => Want::Malformed,
        KeystoreError::UnsupportedKdf(_) => Want::Kdf,
        KeystoreError::UnsupportedCipher(_) => Want::Cipher,
        KeystoreError::ResourcePolicy(_) => Want::Policy,
        KeystoreError::WrongPassword => Want::WrongPassword,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn hostile_metadata_matrix_is_refused_typed_bounded_and_silent() {
    use Want::*;
    let key = random_key();
    let key_hex = alloy_primitives::hex::encode(key);
    let json = keystore_json(&key, PASSWORD);
    let salt = field(&json, "salt");
    let iv = field(&json, "iv");
    let ct = field(&json, "ciphertext");
    let mac = field(&json, "mac");
    // the shape cases start from a file whose KDF is the most expensive the policy allows (pbkdf2
    // c = 2^24, seconds of work): only a refusal before derivation stays under the time bound
    let costly = json.replace(r#""c":1000"#, r#""c":16777216"#);
    assert_ne!(costly, json);
    let pb = |params: &str| with_kdf(&json, "pbkdf2", params);
    let sc = |params: &str| with_kdf(&json, "scrypt", params);
    let s = format!(r#""salt":"{salt}""#);
    let ok = |extra: &str| {
        pb(&format!(
            r#""c":1000,"dklen":32,"prf":"hmac-sha256",{s}{extra}"#
        ))
    };
    assert_eq!(ok(""), json, "the helper rebuilds the original");

    let cases: Vec<(&str, String, Want)> = vec![
        // structure and types
        ("not json", "{".into(), Malformed),
        ("empty", String::new(), Malformed),
        ("array", "[]".into(), Malformed),
        (
            "version 1",
            json.replace(r#""version":3"#, r#""version":1"#),
            Malformed,
        ),
        (
            "version string",
            json.replace(r#""version":3"#, r#""version":"3""#),
            Malformed,
        ),
        (
            "version negative",
            json.replace(r#""version":3"#, r#""version":-3"#),
            Malformed,
        ),
        (
            "version fraction",
            json.replace(r#""version":3"#, r#""version":3.0"#),
            Malformed,
        ),
        (
            "version missing",
            json.replace(r#""version":3,"#, ""),
            Malformed,
        ),
        (
            "duplicate version",
            json.replacen(r#""version":3"#, r#""version":3,"version":3"#, 1),
            Malformed,
        ),
        (
            "crypto and Crypto",
            json.replacen(r#""crypto":"#, r#""Crypto":{},"crypto":"#, 1),
            Malformed,
        ),
        (
            "duplicate mac",
            json.replacen(r#""mac":"#, &format!(r#""mac":"{mac}","mac":"#), 1),
            Malformed,
        ),
        (
            "mac missing",
            json.replace(&format!(r#","mac":"{mac}""#), ""),
            Malformed,
        ),
        (
            "cipher missing",
            json.replace(r#""cipher":"aes-128-ctr","#, ""),
            Malformed,
        ),
        (
            "iv number",
            json.replace(&format!(r#""iv":"{iv}""#), r#""iv":16"#),
            Malformed,
        ),
        (
            "kdf unknown",
            with_kdf(&json, "argon2id", &format!(r#""dklen":32,{s}"#)),
            Kdf,
        ),
        (
            "cipher unknown",
            json.replace("aes-128-ctr", "aes-128-cbc"),
            Cipher,
        ),
        // pbkdf2 numbers
        (
            "c zero",
            pb(&format!(r#""c":0,"dklen":32,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "c negative",
            pb(&format!(r#""c":-1000,"dklen":32,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "c fraction",
            pb(&format!(r#""c":1000.5,"dklen":32,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "c overflow u64",
            pb(&format!(
                r#""c":18446744073709551616,"dklen":32,"prf":"hmac-sha256",{s}"#
            )),
            Malformed,
        ),
        (
            "c huge",
            pb(&format!(
                r#""c":4294967295,"dklen":32,"prf":"hmac-sha256",{s}"#
            )),
            Policy,
        ),
        (
            "c missing",
            pb(&format!(r#""dklen":32,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "c string",
            pb(&format!(r#""c":"1000","dklen":32,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "prf missing",
            pb(&format!(r#""c":1000,"dklen":32,{s}"#)),
            Kdf,
        ),
        (
            "prf sha1",
            pb(&format!(r#""c":1000,"dklen":32,"prf":"hmac-sha1",{s}"#)),
            Kdf,
        ),
        (
            "dklen 31",
            pb(&format!(r#""c":1000,"dklen":31,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "dklen 0",
            pb(&format!(r#""c":1000,"dklen":0,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "dklen huge",
            pb(&format!(
                r#""c":1000,"dklen":4294967296,"prf":"hmac-sha256",{s}"#
            )),
            Malformed,
        ),
        (
            "dklen missing",
            pb(&format!(r#""c":1000,"prf":"hmac-sha256",{s}"#)),
            Malformed,
        ),
        (
            "salt empty",
            pb(r#""c":1000,"dklen":32,"prf":"hmac-sha256","salt":"""#),
            Malformed,
        ),
        (
            "salt missing",
            pb(r#""c":1000,"dklen":32,"prf":"hmac-sha256""#),
            Malformed,
        ),
        (
            "salt 65 bytes",
            pb(&format!(
                r#""c":1000,"dklen":32,"prf":"hmac-sha256","salt":"{}""#,
                "ab".repeat(65)
            )),
            Malformed,
        ),
        (
            "salt not hex",
            pb(r#""c":1000,"dklen":32,"prf":"hmac-sha256","salt":"zz""#),
            Malformed,
        ),
        // scrypt numbers and cost
        (
            "n huge",
            sc(&format!(r#""n":1099511627776,"r":8,"p":1,"dklen":32,{s}"#)),
            Policy,
        ),
        (
            "n*r overflow",
            sc(&format!(
                r#""n":4611686018427387904,"r":4611686018427387904,"p":1,"dklen":32,{s}"#
            )),
            Policy,
        ),
        (
            "n 2^30 r 8 (128 GiB)",
            sc(&format!(r#""n":1073741824,"r":8,"p":1,"dklen":32,{s}"#)),
            Policy,
        ),
        (
            "p huge",
            sc(&format!(r#""n":2,"r":1,"p":4294967295,"dklen":32,{s}"#)),
            Policy,
        ),
        (
            "n not a power of two",
            sc(&format!(r#""n":262145,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "n one",
            sc(&format!(r#""n":1,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "n zero",
            sc(&format!(r#""n":0,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "r zero",
            sc(&format!(r#""n":2,"r":0,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "p zero",
            sc(&format!(r#""n":2,"r":8,"p":0,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "n negative",
            sc(&format!(r#""n":-262144,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "n fraction",
            sc(&format!(r#""n":262144.5,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "n exponent",
            sc(&format!(r#""n":2.62144e5,"r":8,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        (
            "r missing",
            sc(&format!(r#""n":2,"p":1,"dklen":32,{s}"#)),
            Malformed,
        ),
        // exact lengths, all before the KDF
        ("iv 0", set_field(&costly, "iv", ""), Malformed),
        ("iv 15", set_field(&costly, "iv", &iv[..30]), Malformed),
        (
            "iv 17",
            set_field(&costly, "iv", &format!("{iv}00")),
            Malformed,
        ),
        ("iv odd hex", set_field(&costly, "iv", &iv[..31]), Malformed),
        (
            "ciphertext 0",
            set_field(&costly, "ciphertext", ""),
            Malformed,
        ),
        (
            "ciphertext 31",
            set_field(&costly, "ciphertext", &ct[..62]),
            Malformed,
        ),
        (
            "ciphertext 33",
            set_field(&costly, "ciphertext", &format!("{ct}00")),
            Malformed,
        ),
        ("mac 31", set_field(&costly, "mac", &mac[..62]), Malformed),
        (
            "mac 33",
            set_field(&costly, "mac", &format!("{mac}00")),
            Malformed,
        ),
        (
            "oversized file",
            format!("{json}{}", " ".repeat(70_000)),
            Policy,
        ),
        // after the KDF: MAC, scalar and address
        (
            "wrong mac",
            set_field(
                &json,
                "mac",
                &format!(
                    "{}{}",
                    if mac.starts_with('0') { "1" } else { "0" },
                    &mac[1..]
                ),
            ),
            WrongPassword,
        ),
        ("scalar 0", keystore_json(&[0u8; 32], PASSWORD), Malformed),
        ("scalar n", keystore_json(&key_of(N), PASSWORD), Malformed),
        (
            "scalar n+1",
            keystore_json(&key_of(&format!("{}2", &N[..63])), PASSWORD),
            Malformed,
        ),
        (
            "scalar max",
            keystore_json(&[0xff; 32], PASSWORD),
            Malformed,
        ),
        (
            "wrong address",
            json.replacen(
                r#""version":3"#,
                &format!(r#""address":"{}","version":3"#, "11".repeat(20)),
                1,
            ),
            Malformed,
        ),
        (
            "address not hex",
            json.replacen(r#""version":3"#, r#""address":"xdcZZ","version":3"#, 1),
            Malformed,
        ),
    ];

    let rss0 = peak_rss_kib();
    let p = Password::new(PASSWORD.as_bytes().to_vec());
    let mut checked = 0;
    for (name, input, want) in &cases {
        let t = Instant::now();
        let r = decrypt(input.as_bytes(), &p);
        let e = match r {
            Err(e) => e,
            Ok(s) => panic!("{name}: a signer was built for {:?}", s.address()),
        };
        assert_eq!(class(&e), *want, "{name}: {e}");
        checked += 1;
        // shape and cost refusals run no KDF; the rest run one pbkdf2 c=1000
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "{name}: {:?}",
            t.elapsed()
        );
        let text = format!("{e} {e:?}");
        assert!(
            !text.contains(PASSWORD) && !text.contains(&key_hex),
            "{name}: {text}"
        );
        assert!(text.len() < 200, "{name}: unbounded error text");
    }
    println!("{checked} hostile cases refused");
    let grown = peak_rss_kib().saturating_sub(rss0);
    // the largest allowed scrypt (1 GiB) never ran: no case may allocate a KDF buffer
    assert!(grown < 64 * 1024, "peak RSS grew by {grown} KiB");
    // the untouched file still unlocks, so every refusal came from its one change
    assert_eq!(
        decrypt(json.as_bytes(), &p).unwrap().address(),
        decrypt(ok("").as_bytes(), &p).unwrap().address()
    );
}
