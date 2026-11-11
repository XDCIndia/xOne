//! Leak test: sign 1,000 digests with tracing at TRACE level into a capturing subscriber,
//! trigger and format every error and panic path, and assert the private key appears nowhere
//! in the captured text: raw, hex (lower, upper, 0x), or base64 (standard and URL-safe, padded
//! or not). One test only: it installs a process-wide panic hook.
//!
//! Limit: base64 is checked for the whole key at offset 0, which is how a key would be
//! printed; a key embedded at another byte offset of a larger blob would encode differently.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests: failure is a panic

mod common;

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use alloy_primitives::{hex, keccak256};
use base64::Engine;
use common::{keystore_json, random_key, write_file};
use tracing_subscriber::fmt::MakeWriter;
use xone_primitives::{Domain, Signer};
use xone_signer::local_keystore::{decrypt, load};
use xone_signer::nodekey;
use xone_signer::remote::{FencingToken, InProcessSigner, RemoteSigner, SignRequest};
use xone_signer::{AllowInsecurePermissions, Password};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn private_key_never_appears_in_logs_errors_or_panics() {
    let key = random_key();
    let password = "throwaway-password";
    let json = keystore_json(&key, password);
    let dir = tempfile::tempdir().unwrap();
    let (ks, pwf) = (dir.path().join("ks.json"), dir.path().join("pw"));
    write_file(&ks, json.as_bytes(), 0o600);
    write_file(&pwf, password.as_bytes(), 0o600);

    let cap = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(cap.clone())
        .finish();
    let panics = Arc::new(Mutex::new(String::new()));
    let sink = panics.clone();
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        sink.lock().unwrap().push_str(&format!("{info}\n"));
    }));

    let mut text = String::new(); // every error and Debug string we can produce
    let mut address = alloy_primitives::Address::ZERO;
    tracing::subscriber::with_default(sub, || {
        let signer = load(&ks, &pwf, None).unwrap();
        address = signer.address();
        let remote = InProcessSigner::new(load(&ks, &pwf, None).unwrap());
        for i in 0..1000u32 {
            let digest = keccak256(i.to_be_bytes());
            signer.sign(Domain::VoteV2, &digest).unwrap();
            if i % 100 == 0 {
                let req = SignRequest {
                    domain: Domain::SealV2,
                    digest,
                    fence: FencingToken(u64::from(i)),
                };
                remote.sign(&req).unwrap();
                text += &format!(
                    "{:?}\n",
                    remote.sign(&SignRequest {
                        fence: FencingToken(0),
                        ..req.clone()
                    })
                );
            }
        }
        // Error paths, each formatted with Display, Debug and pretty Debug.
        let bad_mac = json.replacen(r#""mac":""#, r#""mac":"00"#, 1);
        let mut errs: Vec<Box<dyn std::fmt::Debug>> = Vec::new();
        let mut shown: Vec<String> = Vec::new();
        macro_rules! record {
            ($r:expr) => {
                match $r {
                    Ok(_) => panic!("expected an error"),
                    Err(e) => {
                        shown.push(format!("{e}"));
                        errs.push(Box::new(e));
                    }
                }
            };
        }
        let pw = |s: &str| Password::new(s.as_bytes().to_vec());
        record!(decrypt(json.as_bytes(), &pw("wrong")));
        record!(decrypt(b"{\"crypto\":", &pw(password)));
        record!(decrypt(bad_mac.as_bytes(), &pw(password)));
        record!(decrypt(
            json.replace("pbkdf2", "argon2").as_bytes(),
            &pw(password)
        ));
        write_file(&pwf, password.as_bytes(), 0o644);
        record!(load(&ks, &pwf, None));
        let nk = dir.path().join("nodekey");
        write_file(&nk, hex::encode(key).as_bytes(), 0o666); // a node key file holding our key
        record!(nodekey::load(&nk, None));
        let _ = nodekey::load(&nk, Some(AllowInsecurePermissions)).unwrap(); // logs the override
        write_file(&nk, &hex::encode(key).as_bytes()[..60], 0o600);
        record!(nodekey::load(&nk, None));
        for e in &errs {
            text += &format!("{e:?}\n{e:#?}\n");
        }
        for s in &shown {
            text += s;
            text.push('\n');
        }
        text += &format!(
            "{signer:?}\n{:?}\n{:#?}\n",
            Password::new(key.to_vec()),
            signer
        );
        // Panic paths: unwrap and expect on errors, and a panic that formats the signer.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            decrypt(json.as_bytes(), &pw("wrong")).unwrap()
        }));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            decrypt(bad_mac.as_bytes(), &pw(password)).expect("unlock")
        }));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            panic!("signer state: {signer:?} {remote:?}")
        }));
    });
    std::panic::set_hook(old_hook);

    let logs = cap.0.lock().unwrap().clone();
    let panic_text = panics.lock().unwrap().clone();
    let mut all = logs.clone();
    all.extend_from_slice(text.as_bytes());
    all.extend_from_slice(panic_text.as_bytes());

    // Controls: the capture works, and the scan can find what it looks for.
    let sign_lines = String::from_utf8_lossy(&logs).matches("local sign").count();
    assert!(sign_lines >= 1000, "captured {sign_lines} sign events");
    assert!(contains(&logs, b"AllowInsecurePermissions override"));
    assert_eq!(panic_text.matches("panicked at").count(), 3);
    let b64 = |e: base64::engine::GeneralPurpose| e.encode(key);
    let needles: Vec<(&str, Vec<u8>)> = vec![
        ("raw", key.to_vec()),
        ("hex", hex::encode(key).into_bytes()),
        ("HEX", hex::encode(key).to_uppercase().into_bytes()),
        (
            "b64",
            b64(base64::engine::general_purpose::STANDARD).into_bytes(),
        ),
        (
            "b64-nopad",
            b64(base64::engine::general_purpose::STANDARD_NO_PAD).into_bytes(),
        ),
        (
            "b64url",
            b64(base64::engine::general_purpose::URL_SAFE).into_bytes(),
        ),
        (
            "b64url-nopad",
            b64(base64::engine::general_purpose::URL_SAFE_NO_PAD).into_bytes(),
        ),
    ];
    // The scan must see a planted key: this is what stops it from passing vacuously.
    for (name, n) in &needles {
        let mut planted = all.clone();
        planted.extend_from_slice(n);
        assert!(contains(&planted, n), "scan control {name}");
        assert!(!contains(&all, n), "private key leaked ({name})");
    }
    // The address is public and does appear: the log carries real content.
    assert!(String::from_utf8_lossy(&logs)
        .to_lowercase()
        .contains(&hex::encode(address)));
    assert!(!contains(&all, password.as_bytes()));
}
