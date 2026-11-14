//! `admin_*` subset (geth `node/api.go` `adminAPI`): `nodeInfo`, `peers`, `addPeer`,
//! `addTrustedPeer`, `removePeer`, `removeTrustedPeer`.

use crate::{
    json::{h256, q},
    server::{Cost, Registry},
};
use serde_json::{json, Value};

pub fn register(r: &mut Registry) {
    r.add("admin_nodeInfo", Cost::C0, |c, _| {
        let n = c.be.net.node_info();
        Ok(json!({
            "id": n.id, "name": n.name, "enode": n.enode, "ip": n.ip, "listenAddr": n.listen_addr,
            "ports": {"discovery": 0, "listener": 0},
            "protocols": {"eth": {"network": q(n.network), "genesis": h256(n.genesis), "head": h256(n.head)}},
        }))
    });
    r.add("admin_peers", Cost::C1, |c, _| {
        Ok(Value::Array(
            c.be.net
                .peers()
                .into_iter()
                .map(|p| {
                    json!({
                        "id": p.id, "name": p.name, "enode": p.enode, "caps": p.caps,
                        "network": {"remoteAddress": p.remote, "inbound": p.inbound, "trusted": p.trusted, "static": false},
                        "protocols": {},
                    })
                })
                .collect(),
        ))
    });
    r.add("admin_addPeer", Cost::C0, |c, p| {
        Ok(json!(c.be.net.add_peer(p.str(0)?, false)?))
    });
    r.add("admin_addTrustedPeer", Cost::C0, |c, p| {
        Ok(json!(c.be.net.add_peer(p.str(0)?, true)?))
    });
    r.add("admin_removePeer", Cost::C0, |c, p| {
        Ok(json!(c.be.net.remove_peer(p.str(0)?, false)?))
    });
    r.add("admin_removeTrustedPeer", Cost::C0, |c, p| {
        Ok(json!(c.be.net.remove_peer(p.str(0)?, true)?))
    });
}
