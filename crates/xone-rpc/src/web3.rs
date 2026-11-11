//! `web3_*`. `web3_clientVersion` is the build's one stamp. `web3_sha3` is not served: the pinned
//! geth-XDC answers `-32601` for it (archive vector `web3_sha3.jsonl`).

use crate::server::{Cost, Registry};
use serde_json::Value;

pub fn register(r: &mut Registry) {
    r.add("web3_clientVersion", Cost::C0, |c, _| {
        Ok(Value::String(c.be.net.client_version()))
    });
}
