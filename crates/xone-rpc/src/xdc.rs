//! The `xdc` namespace of geth-XDC (`eth/backend.go`, `ethapi.NewBlockChainAPI` registered a second time
//! under `xdc`, refs #894 of the oracle repository): every `BlockChainAPI` method that this node serves
//! under `eth_` answers also under `xdc_`, through the one table below (the handlers move, they are not
//! copied), and `getStateHealStatus` (`internal/ethapi/api_xdc.go:643`) under `xdc_` and `eth_`.

use crate::{
    server::{Cost, Ctx, Registry},
    types::{Params, R},
};
use serde_json::json;

/// `(xdc name, eth twin)`: the methods of the oracle `BlockChainAPI` that xOne serves. The transaction,
/// filter and gas-price methods are other services in geth and stay `eth_` only.
const ALIASES: &[(&str, &str)] = &[
    ("xdc_chainId", "eth_chainId"),
    ("xdc_blockNumber", "eth_blockNumber"),
    ("xdc_getBalance", "eth_getBalance"),
    ("xdc_getCode", "eth_getCode"),
    ("xdc_getStorageAt", "eth_getStorageAt"),
    ("xdc_getBlockByNumber", "eth_getBlockByNumber"),
    ("xdc_getBlockByHash", "eth_getBlockByHash"),
    ("xdc_getBlockReceipts", "eth_getBlockReceipts"),
    (
        "xdc_getUncleCountByBlockNumber",
        "eth_getUncleCountByBlockNumber",
    ),
    (
        "xdc_getUncleCountByBlockHash",
        "eth_getUncleCountByBlockHash",
    ),
    ("xdc_call", "eth_call"),
    ("xdc_estimateGas", "eth_estimateGas"),
    ("xdc_getBlockSignersByHash", "eth_getBlockSignersByHash"),
    ("xdc_getBlockSignersByNumber", "eth_getBlockSignersByNumber"),
    ("xdc_getBlockFinalityByHash", "eth_getBlockFinalityByHash"),
    (
        "xdc_getBlockFinalityByNumber",
        "eth_getBlockFinalityByNumber",
    ),
    ("xdc_getMasternodes", "eth_getMasternodes"),
    ("xdc_getCandidateStatus", "eth_getCandidateStatus"),
    ("xdc_getCandidates", "eth_getCandidates"),
];

pub fn register(r: &mut Registry) {
    let mut tmp = Registry::default();
    crate::eth::register(&mut tmp);
    crate::xdc_ext::register(&mut tmp);
    for (x, e) in ALIASES {
        if let Some(entry) = tmp.take(e) {
            r.put(x, entry);
        }
    }
    r.add("xdc_getStateHealStatus", Cost::C0, heal_status);
}

/// `eth_getStateHealStatus`: the same method under `eth`.
pub fn register_eth_heal(r: &mut Registry) {
    r.add("eth_getStateHealStatus", Cost::C0, heal_status);
}

/// The oracle's answer when no heal runs (`GetStateHealStatus`, `SnapHealStatus() == nil`). The node
/// serves RPC after its state is complete; `ponytail:` a live answer needs the snap client's counters
/// published to the RPC backend while it heals.
fn heal_status(_: &Ctx, _: &Params) -> R {
    Ok(json!({
        "enabled": false, "inProgress": false, "mode": "snap-heal", "targetRoot": "",
        "trienodePending": 0, "bytecodePending": 0, "healStartUnix": 0, "lastError": "",
    }))
}
