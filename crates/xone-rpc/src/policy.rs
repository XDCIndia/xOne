//! Who may reach a listener: the `Host` (HTTP virtual hosts) and `Origin` (WebSocket) checks of geth
//! `node/rpcstack.go:443-487` (`virtualHostHandler`) and `rpc/websocket.go:71-127`, `:129-186`
//! (`wsHandshakeValidator`, `ruleAllowsOrigin`). The method allowlist per transport is the registry of
//! the transport's [`crate::server::Rpc`]: a method outside it is `-32601` on that transport.

use crate::server::Registry;
use std::net::IpAddr;

/// Why a request was refused before dispatch (HTTP 403 and a WebSocket handshake 403).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("invalid host specified")]
    InvalidHost,
    #[error("origin not allowed")]
    InvalidOrigin,
    /// An API list names a module that does not exist: the listener does not start, nothing is widened.
    #[error("unknown API namespace `{0}`")]
    UnknownNamespace(String),
}

/// The registry of one transport: only the handlers of `namespaces` (geth `--http.api`, `--ws.api`;
/// `rpc/endpoints.go`, `node/rpcstack.go` `enableRPC`/`enableWS` with `rpc.Server.RegisterName`).
/// `admin`, `debug` and `personal` are served only when the list names them, so the defaults (`eth`, `net`,
/// `web3`) never expose them on any bind address; `personal` has no handlers here. There is no wildcard.
pub fn registry(namespaces: &[String]) -> Result<Registry, Refusal> {
    let mut reg = Registry::default();
    for ns in namespaces {
        match ns.to_ascii_lowercase().as_str() {
            "eth" => {
                crate::eth::register(&mut reg);
                crate::xdc_ext::register(&mut reg);
                crate::xdc::register_eth_heal(&mut reg);
            }
            "xdc" => crate::xdc::register(&mut reg),
            "net" => crate::net::register(&mut reg),
            "web3" => crate::web3::register(&mut reg),
            "txpool" => crate::txpool::register(&mut reg),
            "admin" => crate::admin::register(&mut reg),
            "debug" => crate::debug::register(&mut reg),
            "xdpos" => crate::xdpos::register(&mut reg),
            "personal" => {}
            _ => return Err(Refusal::UnknownNamespace(ns.clone())),
        }
    }
    Ok(reg)
}

/// `--http.vhosts` and `--ws.origins`; `"*"` allows any. The default is geth's: host `localhost`
/// (an IP address is always accepted) and origin `http://localhost`.
#[derive(Debug, Clone)]
pub struct Policy {
    pub vhosts: Vec<String>,
    pub origins: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            vhosts: vec!["localhost".into()],
            origins: vec![],
        }
    }
}

fn host_of(h: &str) -> &str {
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match h.rsplit_once(':') {
        Some((a, p)) if !a.contains(':') && p.bytes().all(|b| b.is_ascii_digit()) => a,
        _ => h,
    }
}

/// `(scheme, hostname, port)` of an origin or an origin rule; a rule without `://` has no scheme.
fn parts(o: &str) -> (String, String, String) {
    let o = o.to_ascii_lowercase();
    let (scheme, rest) = o.split_once("://").unwrap_or(("", &o));
    let rest = rest.split('/').next().unwrap_or("");
    let host = host_of(rest);
    let port = rest
        .strip_prefix(host)
        .and_then(|p| p.strip_prefix(':'))
        .unwrap_or("");
    (scheme.to_owned(), host.to_owned(), port.to_owned())
}

fn rule_allows(rule: &str, origin: &str) -> bool {
    let (rs, rh, rp) = parts(rule);
    let (os, oh, op) = parts(origin);
    (rs.is_empty() || rs == os) && (rh.is_empty() || rh == oh) && (rp.is_empty() || rp == op)
}

impl Policy {
    /// `virtualHostHandler`: no `Host`, an IP address, `*` or a listed name pass.
    pub fn check_host(&self, host: Option<&str>) -> Result<(), Refusal> {
        let Some(h) = host.filter(|h| !h.is_empty()) else {
            return Ok(());
        };
        let name = host_of(h).to_ascii_lowercase();
        let listed = |n: &str| self.vhosts.iter().any(|v| v.eq_ignore_ascii_case(n));
        if name.parse::<IpAddr>().is_ok() || listed("*") || listed(&name) {
            return Ok(());
        }
        Err(Refusal::InvalidHost)
    }

    /// `wsHandshakeValidator`: a request without `Origin` is not a browser and passes.
    pub fn check_origin(&self, origin: Option<&str>) -> Result<(), Refusal> {
        let Some(o) = origin else {
            return Ok(());
        };
        let rules: Vec<&str> = if self.origins.is_empty() {
            vec!["http://localhost"]
        } else {
            self.origins.iter().map(String::as_str).collect()
        };
        if rules.contains(&"*") || rules.iter().any(|r| rule_allows(r, o)) {
            Ok(())
        } else {
            Err(Refusal::InvalidOrigin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rules() {
        let p = Policy {
            vhosts: vec!["node.example".into()],
            origins: vec![],
        };
        assert!(p.check_host(Some("node.example:8545")).is_ok());
        assert!(p.check_host(Some("10.0.0.1:8545")).is_ok());
        assert!(p.check_host(Some("[::1]:8545")).is_ok());
        assert!(p.check_host(None).is_ok());
        assert_eq!(
            p.check_host(Some("evil.example")),
            Err(Refusal::InvalidHost)
        );
        assert!(Policy::default().check_host(Some("localhost:1")).is_ok());
    }

    #[test]
    fn origin_rules() {
        let p = Policy::default();
        assert!(p.check_origin(None).is_ok());
        assert!(p.check_origin(Some("http://localhost:3000")).is_ok());
        assert_eq!(
            p.check_origin(Some("http://evil.example")),
            Err(Refusal::InvalidOrigin)
        );
        let p = Policy {
            vhosts: vec![],
            origins: vec!["https://app.example".into()],
        };
        assert!(p.check_origin(Some("https://APP.example")).is_ok());
        assert!(p.check_origin(Some("http://app.example")).is_err());
    }
}
