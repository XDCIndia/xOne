#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! `xone network init` (issue #223): the generated files load, list every masternode, and the
//! genesis bytes depend only on the inputs.

use alloy_primitives::Address;
use xone_node::{
    dev::chain_with,
    netgen::{init, node_dir, Plan},
    Config,
};

#[test]
fn same_masternodes_give_the_same_genesis_bytes() {
    let v = [
        Address::repeat_byte(1),
        Address::repeat_byte(2),
        Address::repeat_byte(3),
    ];
    let a = chain_with(&v, 2, 4242).unwrap();
    assert_eq!(chain_with(&v, 2, 4242).unwrap(), a);
    assert_ne!(chain_with(&v[..2], 2, 4242).unwrap().0, a.0);
    let g: serde_json::Value = serde_json::from_str(&a.0).unwrap();
    let extra = g["extraData"].as_str().unwrap();
    assert_eq!(extra.len(), 2 + 2 * (32 + 3 * 20 + 65));
    for x in v {
        assert!(extra.contains(&alloy_primitives::hex::encode(x)));
    }
    assert_eq!(g["config"]["chainId"], 4242);
    assert!(chain_with(&[], 2, 4242).is_err());
}

#[test]
fn init_writes_a_loadable_network() {
    let out = std::env::temp_dir().join(format!("xone-netgen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let plan = Plan {
        masternodes: 3,
        period: 2,
        chain_id: 4242,
        base_port: 31000,
    };
    let made = init(&out, &plan).unwrap();
    assert_eq!(made.masternodes.len(), 3);
    assert_eq!(made.enodes.len(), 3);
    assert!(made.genesis_hash.starts_with("0x"));
    let genesis = std::fs::read_to_string(out.join("genesis.json")).unwrap();
    let table = std::fs::read_to_string(out.join("forks.json")).unwrap();
    let spec = xone_chainspec::load_custom(&table).unwrap();
    let g = xone_chainspec::Genesis::from_json(&genesis).unwrap();
    assert_eq!(g.hash(&spec).unwrap().to_string(), made.genesis_hash);
    for k in 1..=3 {
        let d = node_dir(&out, k);
        let c =
            Config::from_toml(&std::fs::read_to_string(d.join("config.toml")).unwrap()).unwrap();
        assert_eq!(c.p2p.port, 31000 + u16::try_from(k - 1).unwrap());
        assert_eq!(
            c.override_genesis.as_deref(),
            Some(out.canonicalize().unwrap().join("genesis.json").as_path())
        );
        let peers: Vec<String> = serde_json::from_str(
            &std::fs::read_to_string(c.datadir.join("static-nodes.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(peers.len(), 2);
        assert!(!peers.contains(&made.enodes[k - 1]));
        // the node key is the one its enode names
        let nk = xone_node::services::net::load_or_create_key(&c.datadir).unwrap();
        let id = alloy_primitives::hex::encode(xone_net::rlpx::peer_id(&nk));
        assert!(made.enodes[k - 1].contains(&id));
        // the validator key belongs to masternode k
        let vk = std::fs::read_to_string(d.join("validator.key")).unwrap();
        let vk =
            secp256k1::SecretKey::from_slice(&alloy_primitives::hex::decode(vk.trim()).unwrap())
                .unwrap();
        assert_eq!(xone_node::netgen::address_of(&vk), made.masternodes[k - 1]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(d.join("validator.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
        }
    }
    // never over an existing network
    assert!(init(&out, &plan).is_err());
    let _ = std::fs::remove_dir_all(&out);
}
