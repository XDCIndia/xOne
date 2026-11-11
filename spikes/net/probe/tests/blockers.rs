use reth_eth_wire::{capability::SharedCapabilities, protocol::Protocol, Capability};
use reth_eth_wire_types::EthVersion;

#[test]
fn eth_100_cannot_be_negotiated() {
    let cap = Capability::new_static("eth", 100);
    let r = SharedCapabilities::try_new(vec![Protocol::new(cap.clone(), 229)], vec![cap]);
    println!("try_new(eth/100) = {:?}", r.as_ref().err());
    assert!(r.is_err());
    assert!(EthVersion::try_from(100u8).is_err());
    assert!(EthVersion::try_from(101u8).is_err());
}
