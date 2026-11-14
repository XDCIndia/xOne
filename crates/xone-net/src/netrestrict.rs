//! `--netrestrict`: the IP networks (CIDR masks) this node talks to (geth `p2p/netutil.Netlist`,
//! `cmd/utils/flags.go:1015`). Discovery drops packets from, and neighbours in, other networks; the
//! dialer refuses them, static and trusted peers included; inbound connections from them are
//! refused before the handshake.

use std::{fmt, net::IpAddr, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRestrict(Vec<(IpAddr, u8)>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid CIDR mask `{0}`")]
pub struct CidrError(pub String);

impl FromStr for NetRestrict {
    type Err = CidrError;
    /// A comma-separated list of `ip/bits` (geth `ParseNetlist`); an empty list is an error.
    fn from_str(s: &str) -> Result<Self, CidrError> {
        let mut v = vec![];
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let bad = || CidrError(part.to_owned());
            let (ip, bits) = part.split_once('/').ok_or_else(bad)?;
            let ip: IpAddr = ip.parse().map_err(|_| bad())?;
            let bits: u8 = bits.parse().map_err(|_| bad())?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            if bits > max {
                return Err(bad());
            }
            v.push((mask(ip, bits), bits));
        }
        if v.is_empty() {
            return Err(CidrError(s.to_owned()));
        }
        Ok(Self(v))
    }
}

impl fmt::Display for NetRestrict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|(ip, b)| format!("{ip}/{b}")).collect();
        f.write_str(&parts.join(","))
    }
}

fn mask(ip: IpAddr, bits: u8) -> IpAddr {
    match ip {
        IpAddr::V4(a) => {
            let m = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            IpAddr::V4((u32::from(a) & m).into())
        }
        IpAddr::V6(a) => {
            let m = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            IpAddr::V6((u128::from(a) & m).into())
        }
    }
}

impl NetRestrict {
    /// Whether `ip` is in one of the networks. An IPv4-mapped IPv6 address counts as IPv4.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.0
            .iter()
            .any(|(net, bits)| net.is_ipv4() == ip.is_ipv4() && mask(ip, *bits) == *net)
    }
}

/// `None` (no restriction) allows every address.
pub fn allowed(r: Option<&NetRestrict>, ip: IpAddr) -> bool {
    r.is_none_or(|r| r.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_lists_parse_and_match() {
        let r: NetRestrict = "10.0.0.0/8, 192.168.1.0/24,fd00::/8".parse().unwrap();
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(r.contains(ip("10.1.2.3")));
        assert!(r.contains(ip("192.168.1.200")));
        assert!(!r.contains(ip("192.168.2.1")));
        assert!(r.contains(ip("fd12::1")));
        assert!(!r.contains(ip("2001:db8::1")));
        assert!(r.contains(ip("::ffff:10.0.0.1")));
        assert!(!r.contains(ip("8.8.8.8")));
        let all: NetRestrict = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains(ip("8.8.8.8")) && !all.contains(ip("::1")));
        // the host bits of the network are ignored, as geth's net.ParseCIDR
        let host: NetRestrict = "127.0.0.1/8".parse().unwrap();
        assert_eq!(host.to_string(), "127.0.0.0/8");
        for bad in [
            "",
            "10.0.0.0",
            "10.0.0.0/33",
            "fd00::/129",
            "x/8",
            "10.0.0.0/a",
        ] {
            assert!(bad.parse::<NetRestrict>().is_err(), "{bad}");
        }
        assert!(allowed(None, ip("8.8.8.8")));
        assert!(!allowed(Some(&r), ip("8.8.8.8")));
    }
}
