//! xOneGo #711: an XDPoS chain with a chain id nobody listed must still get the
//! XDC header format. The format follows the XDPoS config, never the chain id.

use alloy_rlp::{decode_exact, encode};
use xone_chainspec::{apothem, load, Network};
use xone_primitives::Header;

#[test]
fn custom_chain_id_with_xdpos_config_has_the_same_genesis_header() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/generated/forks.json");
    let mut table: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    table["networks"]["apothem"]["chainId"] = "987654321".into();
    let custom = load(Network::Apothem, &table.to_string()).unwrap();
    assert_eq!(custom.chain_id, 987_654_321);
    assert_eq!(custom.xdpos, apothem().xdpos);

    let genesis = Network::Apothem.genesis().unwrap();
    let header = genesis.to_header(&custom).unwrap();
    assert_eq!(header.hash(), apothem().genesis_hash);
    assert_eq!(decode_exact::<Header>(&encode(&header)).unwrap(), header);
}
