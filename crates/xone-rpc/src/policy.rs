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
    /// A transport with a JWT secret got no token, a malformed one, a wrong signature or a stale
    /// `iat` (HTTP 401, a WebSocket handshake 401). The reason is not told to the client.
    #[error("missing or invalid authorization")]
    Unauthorized,
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

/// `--http.vhosts`, `--ws.origins` and `--rpc.jwtsecret`; `"*"` allows any host or origin. The
/// default is geth's: host `localhost` (an IP address is always accepted), origin `http://localhost`,
/// no authentication. `Host`, `Origin` and forwarding headers (`X-Forwarded-For`, `Forwarded`) never
/// grant access: only the JWT does.
#[derive(Clone)]
pub struct Policy {
    pub vhosts: Vec<String>,
    pub origins: Vec<String>,
    /// `--http.corsdomain`: the browser origins an HTTP response grants (geth `newCorsHandler`, which
    /// uses `rs/cors`). Empty: CORS is off and no permission header is sent. Separate from
    /// `origins`, the WebSocket handshake check.
    pub cors: Vec<String>,
    /// The 32-byte HS256 secret. `Some`: every request needs `Authorization: Bearer <jwt>`.
    pub jwt: Option<[u8; 32]>,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("vhosts", &self.vhosts)
            .field("origins", &self.origins)
            .field("cors", &self.cors)
            .field("jwt", &self.jwt.map(|_| "<redacted>"))
            .finish()
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            vhosts: vec!["localhost".into()],
            origins: vec![],
            cors: vec![],
            jwt: None,
        }
    }
}

/// geth `node/jwt_handler.go`: the token's `iat` must be within 60 s of the local clock.
pub const JWT_IAT_SKEW_SECS: u64 = 60;
/// A longer `Authorization` value is refused before it is decoded.
const MAX_TOKEN_BYTES: usize = 1024;

/// HS256 only (geth `jwt.WithValidMethods([HS256])`): `alg: none` and every other algorithm fail.
fn verify_jwt(secret: &[u8; 32], token: &str, now: u64) -> bool {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    if token.len() > MAX_TOKEN_BYTES {
        return false;
    }
    let mut it = token.split('.');
    let (Some(h), Some(p), Some(sig), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return false;
    };
    let json = |s: &str| {
        URL_SAFE_NO_PAD
            .decode(s)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    };
    let (Some(head), Some(claims), Ok(sig)) = (json(h), json(p), URL_SAFE_NO_PAD.decode(sig))
    else {
        return false;
    };
    if head.get("alg").and_then(serde_json::Value::as_str) != Some("HS256") {
        return false;
    }
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(&token.as_bytes()[..h.len() + 1 + p.len()]);
    // constant-time comparison (`hmac` `verify_slice`)
    if mac.verify_slice(&sig).is_err() {
        return false;
    }
    claims
        .get("iat")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|iat| iat.abs_diff(now) <= JWT_IAT_SKEW_SECS)
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

    /// geth `jwtHandler`: with a secret, `Authorization: Bearer <HS256 jwt>` with a fresh `iat` is
    /// required before anything is dispatched; without one every request passes.
    pub fn check_auth(&self, authorization: Option<&str>) -> Result<(), Refusal> {
        let Some(secret) = &self.jwt else {
            return Ok(());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let token = authorization.and_then(|a| a.strip_prefix("Bearer "));
        match token {
            Some(t) if verify_jwt(secret, t.trim(), now) => Ok(()),
            _ => Err(Refusal::Unauthorized),
        }
    }

    /// `rs/cors` origin check for `--http.corsdomain`: the `Access-Control-Allow-Origin` value to
    /// send for `origin`, or `None` (CORS off, no `Origin`, or not allowed). `*` allows any origin
    /// and is sent as `*`; a rule may hold one `*` wildcard (`https://*.example.org`); matching is
    /// case-blind. `null` is allowed only by a rule `null` or `*`. No credentials are ever granted.
    pub fn cors_allow(&self, origin: Option<&str>) -> Option<String> {
        let o = origin?.to_ascii_lowercase();
        if self.cors.iter().any(|r| r == "*") {
            return Some("*".into());
        }
        let ok = self.cors.iter().any(|r| {
            let r = r.to_ascii_lowercase();
            match r.split_once('*') {
                Some((pre, post)) => {
                    o.len() >= pre.len() + post.len() && o.starts_with(pre) && o.ends_with(post)
                }
                None => r == o,
            }
        });
        ok.then_some(o)
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
            ..Policy::default()
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
            cors: vec![],
            jwt: None,
        };
        assert!(p.check_origin(Some("https://APP.example")).is_ok());
        assert!(p.check_origin(Some("http://app.example")).is_err());
    }

    #[test]
    fn cors_rules() {
        assert_eq!(
            Policy::default().cors_allow(Some("https://a.example")),
            None
        );
        let p = Policy {
            cors: vec![
                "https://app.example".into(),
                "https://*.dapp.example".into(),
            ],
            ..Policy::default()
        };
        assert_eq!(
            p.cors_allow(Some("https://APP.example")).as_deref(),
            Some("https://app.example")
        );
        assert!(p.cors_allow(Some("https://x.dapp.example")).is_some());
        assert_eq!(p.cors_allow(Some("https://evil.example")), None);
        assert_eq!(p.cors_allow(Some("null")), None);
        assert_eq!(p.cors_allow(None), None);
        let any = Policy {
            cors: vec!["*".into()],
            ..Policy::default()
        };
        assert_eq!(any.cors_allow(Some("null")).as_deref(), Some("*"));
    }

    /// A token signed with `secret` over `claims`, the way a client builds one.
    pub(crate) fn jwt(secret: &[u8; 32], header: &str, claims: &str) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use hmac::{Hmac, Mac};
        let msg = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret).expect("any key length");
        mac.update(msg.as_bytes());
        format!(
            "{msg}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        )
    }

    #[test]
    fn jwt_rules() {
        let k = [7u8; 32];
        let hs = r#"{"alg":"HS256","typ":"JWT"}"#;
        let now = 1_700_000_000u64;
        let good = jwt(&k, hs, &format!(r#"{{"iat":{now}}}"#));
        assert!(verify_jwt(&k, &good, now));
        assert!(verify_jwt(&k, &good, now + 60));
        assert!(!verify_jwt(&k, &good, now + 61), "stale iat");
        assert!(!verify_jwt(&[8u8; 32], &good, now), "wrong secret");
        assert!(!verify_jwt(&k, &jwt(&k, hs, "{}"), now), "no iat");
        assert!(
            !verify_jwt(&k, &jwt(&k, hs, r#"{"iat":-1}"#), now),
            "negative iat"
        );
        let none = jwt(&k, r#"{"alg":"none"}"#, &format!(r#"{{"iat":{now}}}"#));
        assert!(!verify_jwt(&k, &none, now), "alg none");
        let unsigned = good.rsplit_once('.').unwrap().0.to_owned() + ".";
        assert!(!verify_jwt(&k, &unsigned, now), "empty signature");
        for bad in ["", "a.b", "a.b.c.d", "!!.!!.!!", &"a".repeat(2000)] {
            assert!(!verify_jwt(&k, bad, now), "{bad:.20}");
        }
        let p = Policy {
            jwt: Some(k),
            ..Policy::default()
        };
        assert_eq!(p.check_auth(None), Err(Refusal::Unauthorized));
        assert_eq!(
            p.check_auth(Some(&good)),
            Err(Refusal::Unauthorized),
            "no Bearer"
        );
        assert!(!format!("{p:?}").contains('7'));
        assert!(Policy::default().check_auth(None).is_ok());
    }
}
