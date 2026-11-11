//! `net_*` (geth `internal/ethapi/api.go` `NetAPI`, `p2p/server.go`).

use crate::{
    json::q,
    server::{Cost, Registry},
};
use serde_json::{json, Value};

pub fn register(r: &mut Registry) {
    // `net_version` is the network id as a decimal string.
    r.add("net_version", Cost::C0, |c, _| {
        Ok(Value::String(c.be.net.network_id().to_string()))
    });
    r.add("net_listening", Cost::C0, |c, _| {
        Ok(json!(c.be.net.listening()))
    });
    r.add("net_peerCount", Cost::C0, |c, _| {
        Ok(q(u64::try_from(c.be.net.peer_count()).unwrap_or(u64::MAX)))
    });
}
