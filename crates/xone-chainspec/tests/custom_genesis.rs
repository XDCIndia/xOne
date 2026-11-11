//! `ChainSpec::from_genesis_json`: the `config` block of a genesis file as a chain spec.
#![allow(clippy::unwrap_used)] // test helper functions

use serde_json::Value;
use xone_chainspec::{ChainSpec, Error, ForkBlock, Network};

fn net2040() -> Value {
    serde_json::from_str(Network::Net2040.genesis_json()).unwrap()
}

fn load(v: &Value) -> Result<ChainSpec, Error> {
    ChainSpec::from_genesis_json(&v.to_string())
}

#[test]
fn net2040_genesis_file_is_the_built_in_spec() {
    let spec = ChainSpec::from_genesis_json(Network::Net2040.genesis_json()).unwrap();
    assert_eq!(spec, Network::Net2040.spec());
    // hash read from the net2040 archive, `eth_getBlockByNumber(0)`
    assert_eq!(
        format!("{:?}", spec.genesis_hash),
        "0x8d8ff2e9d8369b7b4af5a84eb010e2b1a358eefa9f971985435d538dda235714"
    );
    assert_eq!(spec.chain_id, 2040);
    assert_eq!(
        spec.tip_fixed_reward,
        ForkBlock::At(16_200),
        "legacy key tipUpgradeRewardBlock"
    );
    assert_eq!(spec.tip_epoch_halving, ForkBlock::At(999_999_999));
    assert_eq!(spec.tip_proportional_reward, ForkBlock::Never);
    assert_eq!(spec.amsterdam, ForkBlock::At(21_600));
}

#[test]
fn a_required_row_that_is_absent_is_a_typed_error() {
    for key in [
        "tipTRC21FeeBlock",
        "gas50xBlock",
        "tipSigningBlock",
        "londonBlock",
    ] {
        let mut g = net2040();
        g["config"].as_object_mut().unwrap().remove(key);
        assert!(
            matches!(load(&g), Err(Error::MissingFork(k)) if k == key),
            "{key}"
        );
        g["config"][key] = Value::Null;
        assert!(
            matches!(load(&g), Err(Error::MissingFork(k)) if k == key),
            "{key} null"
        );
    }
}

#[test]
fn an_optional_row_that_is_absent_is_never() {
    let mut g = net2040();
    for key in ["pragueBlock", "osakaBlock", "amsterdamBlock", "cancunBlock"] {
        g["config"].as_object_mut().unwrap().remove(key);
    }
    let s = load(&g).unwrap();
    assert_eq!(
        (s.prague, s.osaka, s.amsterdam, s.cancun),
        (
            ForkBlock::Never,
            ForkBlock::Never,
            ForkBlock::Never,
            ForkBlock::Never
        )
    );
}

#[test]
fn other_config_errors_are_typed() {
    let mut g = net2040();
    g["config"]["daoForkBlock"] = 5.into();
    assert!(matches!(load(&g), Err(Error::UnmodelledFork(k)) if k == "daoForkBlock"));

    let mut g = net2040();
    g["config"]["tipFixedRewardBlock"] = 17_000.into();
    assert!(matches!(
        load(&g),
        Err(Error::InvalidValue {
            field: "config block",
            ..
        })
    ));
    g["config"]["tipFixedRewardBlock"] = 16_200.into();
    assert!(load(&g).is_ok(), "both keys with the same value");

    let mut g = net2040();
    g["config"]["londonBlock"] = "0x10".into();
    assert!(matches!(
        load(&g),
        Err(Error::InvalidValue {
            field: "config block",
            ..
        })
    ));

    let mut g = net2040();
    g["config"].as_object_mut().unwrap().remove("XDPoS");
    assert!(matches!(load(&g), Err(Error::MissingConfig("XDPoS"))));

    let mut g = net2040();
    g["config"]["XDPoS"]["v2"]
        .as_object_mut()
        .unwrap()
        .remove("allConfigs");
    assert!(matches!(load(&g), Err(Error::Json(_))));

    let mut g = net2040();
    g["config"]["XDPoS"]["SkipV1Validation"] = true.into();
    assert!(matches!(load(&g), Err(Error::Unsupported(_))));

    let mut g = net2040();
    g["config"].as_object_mut().unwrap().remove("chainId");
    assert!(matches!(load(&g), Err(Error::MissingConfig("chainId"))));
}
