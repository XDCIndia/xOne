//! Blocking TCP or TLS connect for the telemetry clients. TLS (rustls, webpki roots) is the
//! default; plaintext needs the caller's explicit private-devnet opt-in and never follows a
//! failed TLS attempt (no silent wss to ws downgrade).

use std::{
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

pub trait Io: Read + Write + Send {}
impl<T: Read + Write + Send> Io for T {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub tls: bool,
    /// The host without brackets: a DNS name, an IPv4 address or an IPv6 address. This form
    /// goes to name resolution and to the TLS server name.
    pub host: String,
    pub port: u16,
    /// The request target: an absolute path, with the query if the URL had one.
    pub path: String,
}

impl Target {
    fn default_port(&self) -> u16 {
        if self.tls {
            443
        } else {
            80
        }
    }

    /// The host as it is written in a URL or a `Host` header: an IPv6 address in brackets.
    pub fn url_host(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// The HTTP `Host` header value (RFC 9110 section 7.2): the port only when it is not the
    /// scheme default.
    pub fn authority(&self) -> String {
        if self.port == self.default_port() {
            self.url_host()
        } else {
            format!("{}:{}", self.url_host(), self.port)
        }
    }
}

/// `[scheme://]host[:port][/path][?query]`. A missing scheme means TLS. An IPv6 host is
/// written in brackets (`[::1]:3000`). Userinfo, control characters, spaces and an unbracketed
/// IPv6 host are refused before any network IO. Errors never echo the input, which may carry
/// credentials.
pub fn parse_target(s: &str, schemes: [&str; 2], default_path: &str) -> Result<Target, String> {
    if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("control character or space in URL".into());
    }
    let (tls, rest) = if let Some(r) = s.strip_prefix(&format!("{}://", schemes[0])) {
        (true, r)
    } else if let Some(r) = s.strip_prefix(&format!("{}://", schemes[1])) {
        (false, r)
    } else if s.contains("://") {
        return Err("unsupported URL scheme".into());
    } else {
        (true, s)
    };
    let (hostport, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = path.split('#').next().unwrap_or("");
    let path = if path.is_empty() {
        default_path.to_owned()
    } else if path.starts_with('?') {
        format!("{default_path}{path}")
    } else {
        path.to_owned()
    };
    if hostport.contains('@') {
        return Err("userinfo in URL authority is not supported".into());
    }
    let default_port = if tls { 443 } else { 80 };
    let port = |p: &str| match p.parse::<u16>() {
        Ok(0) | Err(_) => Err("bad port".to_owned()),
        Ok(n) => Ok(n),
    };
    let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
        let (h, after) = r.split_once(']').ok_or("unclosed '[' in URL host")?;
        if h.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("bad IPv6 address in URL host".into());
        }
        let port = match after {
            "" => default_port,
            _ => port(after.strip_prefix(':').ok_or("bad URL authority")?)?,
        };
        (h, port)
    } else {
        match hostport.split_once(':') {
            Some((_, p)) if p.contains(':') => {
                return Err("IPv6 host must be written in brackets".into())
            }
            Some((h, p)) => (h, port(p)?),
            None => (hostport, default_port),
        }
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    if host.contains(['[', ']']) {
        return Err("bad URL host".into());
    }
    Ok(Target {
        tls,
        host: host.to_owned(),
        port,
        path,
    })
}

/// A connected stream plus a handle on the same socket for changing timeouts later.
pub fn connect(
    t: &Target,
    allow_plaintext: bool,
    connect_timeout: Duration,
) -> io::Result<(Box<dyn Io>, TcpStream)> {
    if !t.tls && !allow_plaintext {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "plaintext telemetry refused: use a TLS URL or set allow_plaintext for a private devnet",
        ));
    }
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    let mut tcp = None;
    for a in (t.host.as_str(), t.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, connect_timeout) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = e,
        }
    }
    let tcp = tcp.ok_or(last)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
    let ctl = tcp.try_clone()?;
    if !t.tls {
        return Ok((Box::new(tcp), ctl));
    }
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(t.host.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad TLS server name"))?;
    let conn = rustls::ClientConnection::new(Arc::new(cfg), name).map_err(io::Error::other)?;
    Ok((Box::new(rustls::StreamOwned::new(conn, tcp)), ctl))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let t = parse_target("stats.example.org", ["wss", "ws"], "/api").unwrap();
        assert_eq!((t.tls, t.port, t.path.as_str()), (true, 443, "/api"));
        let t = parse_target("ws://127.0.0.1:3000", ["wss", "ws"], "/api").unwrap();
        assert_eq!((t.tls, t.port), (false, 3000));
        assert!(parse_target("ftp://x", ["wss", "ws"], "/api").is_err());
        assert!(parse_target("wss://:1", ["wss", "ws"], "/api").is_err());
    }

    #[test]
    fn authorities() {
        let p = |s| parse_target(s, ["https", "http"], "/");
        let t = p("https://10.0.0.1/hb").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.authority()),
            ("10.0.0.1", 443, "10.0.0.1".into())
        );
        let t = p("https://sky.example.org:8443/v1/hb?k=1").unwrap();
        assert_eq!(t.authority(), "sky.example.org:8443");
        assert_eq!(t.path, "/v1/hb?k=1");
        let t = p("http://[::1]:3000/hb").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 3000));
        assert_eq!(t.authority(), "[::1]:3000");
        let t = p("https://[2001:db8::1]").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str()),
            ("2001:db8::1", 443, "/")
        );
        assert_eq!(t.authority(), "[2001:db8::1]");
        let t = p("https://h.example?x=1").unwrap();
        assert_eq!(t.path, "/?x=1");
        let t = p("https://h.example/p#frag").unwrap();
        assert_eq!(t.path, "/p");
    }

    #[test]
    fn bad_authorities_fail_without_echo() {
        let p = |s| parse_target(s, ["https", "http"], "/");
        for bad in [
            "https://::1/x",
            "https://[::1/x",
            "https://[nothost]/x",
            "https://[::1]x/",
            "https://user:secret@h.example/",
            "https://h.example:/",
            "https://h.example:0/",
            "https://h.example/a b",
            "https://h.example/\r\nX: y",
        ] {
            let e = p(bad).unwrap_err();
            assert!(!e.contains("secret") && !e.contains("h.example"), "{e}");
        }
    }

    #[test]
    fn plaintext_refused_without_opt_in() {
        let t = parse_target("ws://127.0.0.1:1", ["wss", "ws"], "/api").unwrap();
        let e = connect(&t, false, Duration::from_millis(50)).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }
}
