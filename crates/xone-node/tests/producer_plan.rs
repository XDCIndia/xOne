#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
//! `--mine` is refused unless a keystore and a password file are named, and always on chain id 50 in
//! this build (temporary, until Gate D); Apothem (51) is allowed. The refusal is typed and comes before anything is opened.
//! Every chain signs behind the ADR-010 quiet window of its chainspec, which configuration can only raise (#118).

use xone_chainspec::Network;
use xone_consensus::v2::safety::Fence;
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
fn mainnet_is_refused_whatever_the_files() {
    assert_eq!(
        plan(&cfg(true, true), &Network::Mainnet.spec()),
        Err(ProducerError::PublicNetwork(50))
    );
}

#[test]
fn apothem_is_allowed_with_its_files_and_keeps_its_window() {
    assert_eq!(
        plan(&cfg(true, true), &Network::Apothem.spec())
            .unwrap()
            .quiet,
        120
    );
}

#[test]
fn a_keystore_and_a_password_file_must_be_named() {
    let devnet = Network::Devnet.spec();
    assert_eq!(
        plan(&cfg(false, true), &devnet),
        Err(ProducerError::NoKeystore)
    );
    assert_eq!(
        plan(&cfg(true, false), &devnet),
        Err(ProducerError::NoPassword)
    );
    let p = plan(&cfg(true, true), &devnet).unwrap();
    assert_eq!(p.keystore.to_str(), Some("/k.json"));
    assert_eq!(p.quiet, 20);
}

/// #118: on net2040 the fence is closed for the 120 ticks of its window and a sighting closes it again.
#[test]
fn the_net2040_fence_is_closed_for_its_quiet_window() {
    let p = plan(&cfg(true, true), &Network::Net2040.spec()).unwrap();
    assert_eq!(p.quiet, 120);
    let mut f = Fence::start(p.quiet);
    for _ in 1..p.quiet {
        f.tick();
        assert!(!f.open());
    }
    f.tick();
    assert!(f.open());
    f.sighted();
    assert!(!f.open());
}

#[test]
fn the_quiet_window_can_only_be_raised() {
    let net2040 = Network::Net2040.spec();
    let mut c = cfg(true, true);
    c.set("miner.quietwindow", "0").unwrap();
    assert_eq!(
        plan(&c, &net2040),
        Err(ProducerError::QuietWindowLowered { have: 0, min: 120 })
    );
    c.set("miner.quietwindow", "119").unwrap();
    assert!(plan(&c, &net2040).is_err());
    c.set("miner.quietwindow", "300").unwrap();
    assert_eq!(plan(&c, &net2040).unwrap().quiet, 300);
}
