#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
//! `--mine` is refused unless a keystore and a password file are named, and always on chain ids 50 and 51 in
//! this build (temporary, until Gate D). The refusal is typed and comes before anything is opened.

use xone_node::{
    services::producer::{plan, ProducerError},
    Config,
};

fn cfg(keystore: bool, password: bool) -> Config {
    let mut c = Config::default();
    c.set("mine", "true").unwrap();
    if keystore {
        c.set("keystore", "/k.json").unwrap();
    }
    if password {
        c.set("password", "/pw").unwrap();
    }
    c
}

#[test]
fn public_networks_are_refused_whatever_the_files() {
    for id in [50, 51] {
        assert_eq!(
            plan(&cfg(true, true), id),
            Err(ProducerError::PublicNetwork(id))
        );
    }
}

#[test]
fn a_keystore_and_a_password_file_must_be_named() {
    assert_eq!(
        plan(&cfg(false, true), 5551),
        Err(ProducerError::NoKeystore)
    );
    assert_eq!(
        plan(&cfg(true, false), 5551),
        Err(ProducerError::NoPassword)
    );
    let p = plan(&cfg(true, true), 5551).unwrap();
    assert_eq!(p.keystore.to_str(), Some("/k.json"));
}
