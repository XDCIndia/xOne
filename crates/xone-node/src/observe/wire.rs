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
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// `[scheme://]host[:port][/path]`. A missing scheme means TLS. Errors never echo the input,
/// which may carry credentials.
pub fn parse_target(s: &str, schemes: [&str; 2], default_path: &str) -> Result<Target, String> {
    let (tls, rest) = if let Some(r) = s.strip_prefix(&format!("{}://", schemes[0])) {
        (true, r)
    } else if let Some(r) = s.strip_prefix(&format!("{}://", schemes[1])) {
        (false, r)
    } else if s.contains("://") {
        return Err("unsupported URL scheme".into());
    } else {
        (true, s)
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, default_path),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port".to_owned())?),
        None => (hostport, if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err("empty host".into());
    }
    Ok(Target {
        tls,
        host: host.to_owned(),
        port,
        path: path.to_owned(),
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
    fn plaintext_refused_without_opt_in() {
        let t = parse_target("ws://127.0.0.1:1", ["wss", "ws"], "/api").unwrap();
        let e = connect(&t, false, Duration::from_millis(50)).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }
}
