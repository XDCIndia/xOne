//! Discovery v4 with the XDC ping: UDP packets signed with the node key, ping as packet type 5,
//! no ENR. Sans-IO: [`Discv4`] takes datagrams and a clock and writes datagrams to a
//! [`Transport`], so it runs over a fake network in tests and over a `UdpSocket` in the node.
//! geth-XDC: `p2p/discover/v4wire/v4wire.go:38-53` (types, `UseXDCPing`), `:113-114`
//! (`MaxNeighbors` 12), `:215-300` (packet frame), `p2p/discover/v4_udp.go:52-64` (timeouts,
//! 1280-byte packets), `:596-630` (bond, relay rules), `:672-784` (ping, pong, findnode),
//! `p2p/discover/table.go:45-56` (17 buckets of 16).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use alloy_primitives::{keccak256, Bytes, B256, B512};
use alloy_rlp::{Decodable, Encodable, RlpEncodable};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, SecretKey, SECP256K1};

use crate::rlpx::{peer_id, PeerId};

pub const PING_XDC: u8 = 5;
const PING: u8 = 1;
const PONG: u8 = 2;
const FINDNODE: u8 = 3;
const NEIGHBORS: u8 = 4;
/// The ENR response. ENR is not required, so it is never answered or sent. Type 5 is the XDC
/// ping, never an ENR request.
const ENR_RESPONSE: u8 = 6;

pub const MAX_PACKET: usize = 1280;
pub const MAX_NEIGHBORS: usize = 12;
/// Seconds a packet we send stays valid (`v4_udp.go:53`).
pub const EXPIRATION_S: u64 = 20;
/// How long a ping or findnode waits for its reply (`v4_udp.go:52`).
pub const RESP_TIMEOUT_MS: u64 = 500;
pub const BUCKET_SIZE: usize = 16;
const BUCKETS: usize = 17;
/// Log distance of the closest bucket (`table.go:51-53`: 256 - 17).
const MIN_DISTANCE: usize = 239;
/// Pings and findnodes in flight; more are dropped, not queued.
pub const MAX_PENDING: usize = 256;
const HEAD: usize = 32 + 65;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscError {
    #[error("packet too small or too large ({0} bytes)")]
    Size(usize),
    #[error("bad hash")]
    BadHash,
    #[error("bad signature")]
    BadSignature,
    #[error("unknown packet type {0}")]
    UnknownType(u8),
    #[error("packet type {0} is not supported (no ENR)")]
    Unsupported(u8),
    #[error("rlp: {0}")]
    Rlp(String),
    #[error("expired")]
    Expired,
    #[error("unsolicited reply")]
    Unsolicited,
    #[error("unknown node (no endpoint proof)")]
    UnknownNode,
    #[error("packet from ourselves")]
    SelfPacket,
    #[error("sender outside --netrestrict")]
    Restricted,
}

impl From<alloy_rlp::Error> for DiscError {
    fn from(e: alloy_rlp::Error) -> Self {
        DiscError::Rlp(e.to_string())
    }
}

/// An endpoint on the wire: `[ip, udp, tcp]`, `ip` of 4 or 16 bytes.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable)]
pub struct Endpoint {
    pub ip: Bytes,
    pub udp: u16,
    pub tcp: u16,
}

impl Endpoint {
    pub fn new(addr: SocketAddr, tcp: u16) -> Self {
        let ip = match addr.ip() {
            IpAddr::V4(a) => Bytes::copy_from_slice(&a.octets()),
            IpAddr::V6(a) => Bytes::copy_from_slice(&a.octets()),
        };
        Self {
            ip,
            udp: addr.port(),
            tcp,
        }
    }
}

fn parse_ip(b: &[u8]) -> Option<IpAddr> {
    match b.len() {
        4 => <[u8; 4]>::try_from(b)
            .ok()
            .map(|o| IpAddr::V4(Ipv4Addr::from(o))),
        16 => <[u8; 16]>::try_from(b)
            .ok()
            .map(|o| IpAddr::V6(Ipv6Addr::from(o))),
        _ => None,
    }
}

impl Decodable for Endpoint {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let p = &mut open_list(buf)?;
        let e = Endpoint {
            ip: Decodable::decode(p)?,
            udp: Decodable::decode(p)?,
            tcp: Decodable::decode(p)?,
        };
        end_list(p)?;
        Ok(e)
    }
}

/// A node entry of a Neighbors packet: `[ip, udp, tcp, id]`.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable)]
pub struct WireNode {
    pub ip: Bytes,
    pub udp: u16,
    pub tcp: u16,
    pub id: B512,
}

impl Decodable for WireNode {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let p = &mut open_list(buf)?;
        let n = WireNode {
            ip: Decodable::decode(p)?,
            udp: Decodable::decode(p)?,
            tcp: Decodable::decode(p)?,
            id: Decodable::decode(p)?,
        };
        end_list(p)?;
        Ok(n)
    }
}

/// The payload of the next RLP list, advancing `buf` past it.
fn open_list<'a>(buf: &mut &'a [u8]) -> alloy_rlp::Result<&'a [u8]> {
    let h = alloy_rlp::Header::decode(buf)?;
    if !h.list {
        return Err(alloy_rlp::Error::UnexpectedString);
    }
    let (payload, rest) = buf
        .split_at_checked(h.payload_length)
        .ok_or(alloy_rlp::Error::InputTooShort)?;
    *buf = rest;
    Ok(payload)
}

fn end_list(p: &[u8]) -> alloy_rlp::Result<()> {
    if p.is_empty() {
        Ok(())
    } else {
        Err(alloy_rlp::Error::ListLengthMismatch {
            expected: 0,
            got: p.len(),
        })
    }
}

/// The packets we speak. Fields after the documented ones (`ENRSeq`, forward-compatibility
/// tail) are ignored on decode and never sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Sent as type 5 (XDC); type 1 is accepted on input as well (`v4wire.go:245-260`).
    Ping {
        version: u64,
        from: Endpoint,
        to: Endpoint,
        expiration: u64,
    },
    Pong {
        to: Endpoint,
        reply_tok: Bytes,
        expiration: u64,
    },
    FindNode {
        target: B512,
        expiration: u64,
    },
    Neighbors {
        nodes: Vec<WireNode>,
        expiration: u64,
    },
}

impl Packet {
    pub fn kind(&self) -> u8 {
        match self {
            Packet::Ping { .. } => PING_XDC,
            Packet::Pong { .. } => PONG,
            Packet::FindNode { .. } => FINDNODE,
            Packet::Neighbors { .. } => NEIGHBORS,
        }
    }

    fn body(&self) -> Vec<u8> {
        let mut p = Vec::new();
        match self {
            Packet::Ping {
                version,
                from,
                to,
                expiration,
            } => {
                version.encode(&mut p);
                from.encode(&mut p);
                to.encode(&mut p);
                expiration.encode(&mut p);
            }
            Packet::Pong {
                to,
                reply_tok,
                expiration,
            } => {
                to.encode(&mut p);
                reply_tok.encode(&mut p);
                expiration.encode(&mut p);
            }
            Packet::FindNode { target, expiration } => {
                target.encode(&mut p);
                expiration.encode(&mut p);
            }
            Packet::Neighbors { nodes, expiration } => {
                nodes.encode(&mut p);
                expiration.encode(&mut p);
            }
        }
        let mut out = Vec::with_capacity(p.len() + 3);
        alloy_rlp::Header {
            list: true,
            payload_length: p.len(),
        }
        .encode(&mut out);
        out.extend(p);
        out
    }
}

/// `hash(32) ++ signature(65) ++ type ++ rlp(body)`, signed over `keccak(type ++ body)`.
/// Returns the datagram and its hash (the reply token of the pong).
pub fn encode(key: &SecretKey, packet: &Packet) -> (Vec<u8>, B256) {
    let mut typed = vec![packet.kind()];
    typed.extend(packet.body());
    let digest = Message::from_digest(keccak256(&typed).0);
    let (rid, sig) = SECP256K1
        .sign_ecdsa_recoverable(&digest, key)
        .serialize_compact();
    let mut out = vec![0u8; 32];
    out.extend_from_slice(&sig);
    out.push(u8::try_from(i32::from(rid)).unwrap_or(0));
    out.extend(typed);
    let hash = keccak256(&out[32..]);
    out[..32].copy_from_slice(hash.as_slice());
    (out, hash)
}

/// Check the hash and the signature, recover the sender, decode the packet. Expiry is the
/// caller's: it needs a clock.
pub fn decode(data: &[u8]) -> Result<(Packet, PeerId, B256), DiscError> {
    if data.len() <= HEAD || data.len() > MAX_PACKET {
        return Err(DiscError::Size(data.len()));
    }
    let (hash, rest) = data.split_at(32);
    if keccak256(rest).as_slice() != hash {
        return Err(DiscError::BadHash);
    }
    let (sig, typed) = rest.split_at(65);
    let rid = RecoveryId::try_from(i32::from(sig[64])).map_err(|_| DiscError::BadSignature)?;
    let sig =
        RecoverableSignature::from_compact(&sig[..64], rid).map_err(|_| DiscError::BadSignature)?;
    let msg = Message::from_digest(keccak256(typed).0);
    let key = SECP256K1
        .recover_ecdsa(&msg, &sig)
        .map_err(|_| DiscError::BadSignature)?;
    let from = reth_network_peers::pk2id(&key);
    let mut body = &typed[1..];
    let packet = match typed[0] {
        PING | PING_XDC => {
            let p = &mut open_list(&mut body)?;
            Packet::Ping {
                version: Decodable::decode(p)?,
                from: Decodable::decode(p)?,
                to: Decodable::decode(p)?,
                expiration: Decodable::decode(p)?,
            }
        }
        PONG => {
            let p = &mut open_list(&mut body)?;
            Packet::Pong {
                to: Decodable::decode(p)?,
                reply_tok: Decodable::decode(p)?,
                expiration: Decodable::decode(p)?,
            }
        }
        FINDNODE => {
            let p = &mut open_list(&mut body)?;
            Packet::FindNode {
                target: Decodable::decode(p)?,
                expiration: Decodable::decode(p)?,
            }
        }
        NEIGHBORS => {
            let p = &mut open_list(&mut body)?;
            Packet::Neighbors {
                nodes: Decodable::decode(p)?,
                expiration: Decodable::decode(p)?,
            }
        }
        t @ ENR_RESPONSE => return Err(DiscError::Unsupported(t)),
        t => return Err(DiscError::UnknownType(t)),
    };
    Ok((packet, from, B256::from_slice(hash)))
}

fn expired(expiration: u64, now_ms: u64) -> bool {
    expiration < now_ms / 1000
}

/// A discovered node: where to send UDP, and the TCP port of RLPx.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: PeerId,
    pub addr: SocketAddr,
    pub tcp: u16,
}

fn log_dist(a: &B256, b: &B256) -> usize {
    let mut lz = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        let v = x ^ y;
        if v == 0 {
            lz += 8;
        } else {
            lz += usize::try_from(v.leading_zeros()).unwrap_or(8);
            break;
        }
    }
    256 - lz
}

/// Nodes by log distance from us: 17 buckets of at most 16, so at most 272 nodes. A node is in
/// the table only after a pong proved its endpoint. A full bucket keeps its old nodes and
/// refuses the new one. ponytail: no revalidation or eviction of dead entries; add it with the
/// periodic refresh when the discovery loop is wired.
#[derive(Debug)]
pub struct Table {
    local: B256,
    buckets: [Vec<Node>; BUCKETS],
}

impl Table {
    pub fn new(local: &PeerId) -> Self {
        Self {
            local: keccak256(local),
            buckets: Default::default(),
        }
    }

    fn bucket(&self, id: &B256) -> Option<usize> {
        match log_dist(&self.local, id) {
            0 => None,
            d => Some(d.saturating_sub(MIN_DISTANCE).min(BUCKETS - 1)),
        }
    }

    /// Adds or refreshes. `false`: the node is us or its bucket is full.
    pub fn insert(&mut self, node: Node) -> bool {
        let Some(b) = self.bucket(&keccak256(node.id)) else {
            return false;
        };
        let bucket = &mut self.buckets[b];
        if let Some(e) = bucket.iter_mut().find(|e| e.id == node.id) {
            *e = node;
            return true;
        }
        if bucket.len() >= BUCKET_SIZE {
            return false;
        }
        bucket.push(node);
        true
    }

    pub fn len(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &PeerId) -> Option<&Node> {
        self.buckets.iter().flatten().find(|n| n.id == *id)
    }

    /// The `n` nodes closest to `target` (a keccak of a node id), nearest first.
    pub fn closest(&self, target: &B256, n: usize) -> Vec<Node> {
        let mut all: Vec<(B256, &Node)> = self
            .buckets
            .iter()
            .flatten()
            .map(|e| {
                let mut x = keccak256(e.id);
                for (a, b) in x.0.iter_mut().zip(target.iter()) {
                    *a ^= b;
                }
                (x, e)
            })
            .collect();
        all.sort_by_key(|(d, _)| *d);
        all.into_iter().take(n).map(|(_, e)| e.clone()).collect()
    }
}

/// Where datagrams go. Delivery is best effort, as UDP is.
pub trait Transport {
    fn send(&mut self, to: SocketAddr, data: Vec<u8>);
}

impl Transport for tokio::net::UdpSocket {
    fn send(&mut self, to: SocketAddr, data: Vec<u8>) {
        // A full socket buffer drops the datagram, like the network would.
        let _ = self.try_send_to(&data, to);
    }
}

/// Test and replay transport: collects `(destination, datagram)`.
impl Transport for Vec<(SocketAddr, Vec<u8>)> {
    fn send(&mut self, to: SocketAddr, data: Vec<u8>) {
        self.push((to, data));
    }
}

struct PendingPing {
    hash: B256,
    addr: SocketAddr,
    tcp: u16,
    sent_ms: u64,
}

struct PendingFind {
    addr: SocketAddr,
    sent_ms: u64,
    received: usize,
}

/// geth `netutil.CheckRelayAddr` (`p2p/netutil/net.go:197-222`) for the cases that matter: a
/// relayed address is never unspecified or multicast; loopback and private addresses are only
/// relayed by a host on such a network.
fn relay_ok(sender: IpAddr, addr: IpAddr) -> bool {
    let lan = |ip: IpAddr| match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00,
    };
    !addr.is_unspecified() && !addr.is_multicast() && (!lan(addr) || lan(sender))
}

pub struct Discv4 {
    key: SecretKey,
    id: PeerId,
    local: SocketAddr,
    tcp_port: u16,
    table: Table,
    pings: HashMap<PeerId, PendingPing>,
    finds: HashMap<PeerId, PendingFind>,
    restrict: Option<crate::netrestrict::NetRestrict>,
}

impl Discv4 {
    /// Talk only to the networks of `r` (`--netrestrict`): packets from other addresses are
    /// dropped, neighbours in them are not pinged, and no ping is sent to them.
    pub fn with_restrict(mut self, r: Option<crate::netrestrict::NetRestrict>) -> Self {
        self.restrict = r;
        self
    }

    fn allowed(&self, ip: IpAddr) -> bool {
        crate::netrestrict::allowed(self.restrict.as_ref(), ip)
    }

    /// `local` is the UDP address we advertise, `tcp_port` the RLPx port.
    pub fn new(key: SecretKey, local: SocketAddr, tcp_port: u16) -> Self {
        let id = peer_id(&key);
        Self {
            key,
            id,
            local,
            tcp_port,
            table: Table::new(&id),
            pings: HashMap::new(),
            finds: HashMap::new(),
            restrict: None,
        }
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    fn expiration(now_ms: u64) -> u64 {
        now_ms / 1000 + EXPIRATION_S
    }

    /// Send a ping (type 5) to bond with a node. Does nothing when [`MAX_PENDING`] pings wait.
    pub fn ping(&mut self, now_ms: u64, to: SocketAddr, to_id: PeerId, t: &mut impl Transport) {
        self.ping_with_tcp(now_ms, to, to_id, to.port(), t);
    }

    fn ping_with_tcp(
        &mut self,
        now_ms: u64,
        to: SocketAddr,
        to_id: PeerId,
        tcp: u16,
        t: &mut impl Transport,
    ) {
        if !self.allowed(to.ip()) {
            return;
        }
        if self.pings.len() >= MAX_PENDING && !self.pings.contains_key(&to_id) {
            return;
        }
        let (data, hash) = encode(
            &self.key,
            &Packet::Ping {
                version: 4,
                from: Endpoint::new(self.local, self.tcp_port),
                to: Endpoint::new(to, 0),
                expiration: Self::expiration(now_ms),
            },
        );
        self.pings.insert(
            to_id,
            PendingPing {
                hash,
                addr: to,
                tcp,
                sent_ms: now_ms,
            },
        );
        t.send(to, data);
    }

    /// Ask a bonded node for the nodes closest to `target`.
    pub fn find_node(&mut self, now_ms: u64, to: &Node, target: PeerId, t: &mut impl Transport) {
        if self.finds.len() >= MAX_PENDING && !self.finds.contains_key(&to.id) {
            return;
        }
        let (data, _) = encode(
            &self.key,
            &Packet::FindNode {
                target,
                expiration: Self::expiration(now_ms),
            },
        );
        self.finds.insert(
            to.id,
            PendingFind {
                addr: to.addr,
                sent_ms: now_ms,
                received: 0,
            },
        );
        t.send(to.addr, data);
    }

    /// Forget requests older than [`RESP_TIMEOUT_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        self.pings
            .retain(|_, p| now_ms.saturating_sub(p.sent_ms) < RESP_TIMEOUT_MS);
        self.finds
            .retain(|_, f| now_ms.saturating_sub(f.sent_ms) < RESP_TIMEOUT_MS);
    }

    /// Process one datagram. An `Err` is a dropped packet; nothing is sent for it.
    pub fn handle(
        &mut self,
        now_ms: u64,
        from: SocketAddr,
        data: &[u8],
        t: &mut impl Transport,
    ) -> Result<(), DiscError> {
        if !self.allowed(from.ip()) {
            return Err(DiscError::Restricted);
        }
        let (packet, id, hash) = decode(data)?;
        if id == self.id {
            return Err(DiscError::SelfPacket);
        }
        match packet {
            Packet::Ping {
                from: ep,
                expiration,
                ..
            } => {
                if expired(expiration, now_ms) {
                    return Err(DiscError::Expired);
                }
                let (pong, _) = encode(
                    &self.key,
                    &Packet::Pong {
                        to: Endpoint::new(from, ep.tcp),
                        reply_tok: Bytes::copy_from_slice(hash.as_slice()),
                        expiration: Self::expiration(now_ms),
                    },
                );
                t.send(from, pong);
                // Bond back unless we already know this endpoint (`v4_udp.go:704-711`).
                if self.table.get(&id).is_none_or(|n| n.addr != from) {
                    self.ping_with_tcp(now_ms, from, id, ep.tcp, t);
                }
                Ok(())
            }
            Packet::Pong {
                reply_tok,
                expiration,
                ..
            } => {
                if expired(expiration, now_ms) {
                    return Err(DiscError::Expired);
                }
                let p = self.pings.get(&id).ok_or(DiscError::Unsolicited)?;
                if p.addr.ip() != from.ip() || reply_tok.as_ref() != p.hash.as_slice() {
                    return Err(DiscError::Unsolicited);
                }
                let node = Node {
                    id,
                    addr: p.addr,
                    tcp: p.tcp,
                };
                self.pings.remove(&id);
                self.table.insert(node);
                Ok(())
            }
            Packet::FindNode { target, expiration } => {
                if expired(expiration, now_ms) {
                    return Err(DiscError::Expired);
                }
                // No endpoint proof, no answer: a findnode from a spoofed address must not make
                // us send a larger packet to the victim (`v4_udp.go:735-752`).
                if self.table.get(&id).is_none_or(|n| n.addr.ip() != from.ip()) {
                    return Err(DiscError::UnknownNode);
                }
                let nodes: Vec<WireNode> = self
                    .table
                    .closest(&keccak256(target), BUCKET_SIZE)
                    .into_iter()
                    .filter(|n| relay_ok(from.ip(), n.addr.ip()))
                    .map(|n| {
                        let e = Endpoint::new(n.addr, n.tcp);
                        WireNode {
                            ip: e.ip,
                            udp: e.udp,
                            tcp: e.tcp,
                            id: n.id,
                        }
                    })
                    .collect();
                let exp = Self::expiration(now_ms);
                let mut chunks: Vec<&[WireNode]> = nodes.chunks(MAX_NEIGHBORS).collect();
                if chunks.is_empty() {
                    chunks.push(&[]);
                }
                for c in chunks {
                    let (p, _) = encode(
                        &self.key,
                        &Packet::Neighbors {
                            nodes: c.to_vec(),
                            expiration: exp,
                        },
                    );
                    t.send(from, p);
                }
                Ok(())
            }
            Packet::Neighbors { nodes, expiration } => {
                if expired(expiration, now_ms) {
                    return Err(DiscError::Expired);
                }
                let f = self.finds.get_mut(&id).ok_or(DiscError::Unsolicited)?;
                if f.addr.ip() != from.ip() {
                    return Err(DiscError::Unsolicited);
                }
                f.received += nodes.len();
                if f.received >= BUCKET_SIZE {
                    self.finds.remove(&id);
                }
                for n in nodes.into_iter().take(BUCKET_SIZE) {
                    let Some(ip) = parse_ip(&n.ip) else { continue };
                    // geth: UDP port above 1024, relayable address, a real key (`v4_udp.go:610-625`)
                    if n.udp <= 1024
                        || n.id == self.id
                        || !self.allowed(ip)
                        || !relay_ok(from.ip(), ip)
                        || self.table.get(&n.id).is_some()
                    {
                        continue;
                    }
                    self.ping_with_tcp(now_ms, SocketAddr::new(ip, n.udp), n.id, n.tcp, t);
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const NOW: u64 = 1_700_000_000_000;

    fn key(i: u8) -> SecretKey {
        SecretKey::from_slice(&[i; 32]).unwrap()
    }

    fn addr(i: u8) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, i)),
            30_000 + u16::from(i),
        )
    }

    /// Nodes `1..=n` on a fake network. `step` delivers one datagram.
    struct Net {
        nodes: HashMap<SocketAddr, Discv4>,
        queue: VecDeque<(SocketAddr, SocketAddr, Vec<u8>)>, // from, to, data
        log: Vec<(SocketAddr, SocketAddr, u8)>,             // from, to, packet type
        errors: Vec<(SocketAddr, DiscError)>,
    }

    impl Net {
        fn new(n: u8) -> Self {
            let nodes = (1..=n)
                .map(|i| (addr(i), Discv4::new(key(i), addr(i), 40_000 + u16::from(i))))
                .collect();
            Net {
                nodes,
                queue: VecDeque::new(),
                log: Vec::new(),
                errors: Vec::new(),
            }
        }

        fn id(&self, i: u8) -> PeerId {
            self.nodes[&addr(i)].id()
        }

        fn flush(&mut self, from: SocketAddr, out: Vec<(SocketAddr, Vec<u8>)>) {
            for (to, data) in out {
                self.log.push((from, to, data[HEAD]));
                self.queue.push_back((from, to, data));
            }
        }

        fn ping(&mut self, a: u8, b: u8) {
            let id = self.id(b);
            let mut out = Vec::new();
            self.nodes
                .get_mut(&addr(a))
                .unwrap()
                .ping(NOW, addr(b), id, &mut out);
            self.flush(addr(a), out);
        }

        fn find(&mut self, a: u8, b: u8, target: PeerId) {
            let node = self.nodes[&addr(a)]
                .table()
                .get(&self.id(b))
                .unwrap()
                .clone();
            let mut out = Vec::new();
            self.nodes
                .get_mut(&addr(a))
                .unwrap()
                .find_node(NOW, &node, target, &mut out);
            self.flush(addr(a), out);
        }

        fn run(&mut self) {
            while let Some((from, to, data)) = self.queue.pop_front() {
                let mut out = Vec::new();
                if let Err(e) = self
                    .nodes
                    .get_mut(&to)
                    .unwrap()
                    .handle(NOW, from, &data, &mut out)
                {
                    self.errors.push((to, e));
                }
                self.flush(to, out);
            }
        }
    }

    #[test]
    fn ping_is_type_5_and_bonding_fills_both_tables() {
        let mut net = Net::new(2);
        net.ping(1, 2);
        net.run();
        // ping(5), pong(2), ping back(5), pong(2): no packet of type 1, 6 or 7 is ever sent
        let kinds: Vec<u8> = net.log.iter().map(|l| l.2).collect();
        assert_eq!(kinds, [PING_XDC, PONG, PING_XDC, PONG]);
        assert!(net.errors.is_empty(), "{:?}", net.errors);
        let (id1, id2) = (net.id(1), net.id(2));
        let n2 = net.nodes[&addr(1)].table().get(&id2).unwrap();
        assert_eq!((n2.addr, n2.tcp), (addr(2), addr(2).port())); // dialer: tcp defaults to udp port
        let n1 = net.nodes[&addr(2)].table().get(&id1).unwrap();
        assert_eq!((n1.addr, n1.tcp), (addr(1), 40_001)); // listener learns the tcp port of the ping
    }

    #[test]
    fn findnode_needs_a_bond_and_returns_neighbors_that_get_pinged() {
        let mut net = Net::new(4);
        // 1 bonds with 2, 3 bonds with 2; 4 never bonds
        net.ping(1, 2);
        net.ping(3, 2);
        net.run();
        net.log.clear();
        // 4 asks 2 without a bond: dropped, nothing sent back
        let id2 = net.id(2);
        let mut out = Vec::new();
        let (data, _) = encode(
            &key(4),
            &Packet::FindNode {
                target: net.id(1),
                expiration: NOW / 1000 + 20,
            },
        );
        net.nodes
            .get_mut(&addr(2))
            .unwrap()
            .handle(NOW, addr(4), &data, &mut out)
            .unwrap_err();
        assert!(out.is_empty());
        // 1 asks 2: neighbors carry 1 and 3 (2 knows both), 1 pings 3 and bonds
        let t = net.id(3);
        net.find(1, 2, t);
        net.run();
        let kinds: Vec<u8> = net.log.iter().map(|l| l.2).collect();
        assert_eq!(&kinds[..2], &[FINDNODE, NEIGHBORS]);
        assert!(net.nodes[&addr(1)].table().get(&net.id(3)).is_some());
        assert!(net.nodes[&addr(3)].table().get(&net.id(1)).is_some());
        let _ = id2;
    }

    #[test]
    fn bad_packets_are_dropped_without_a_reply() {
        let mut net = Net::new(2);
        let (good, _) = encode(
            &key(1),
            &Packet::Ping {
                version: 4,
                from: Endpoint::new(addr(1), 1),
                to: Endpoint::new(addr(2), 0),
                expiration: NOW / 1000 + 20,
            },
        );
        let mut out: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        let b = net.nodes.get_mut(&addr(2)).unwrap();
        // flipped payload byte: bad hash; flipped hash byte: bad hash; truncated and oversized
        let mut bad = good.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(
            b.handle(NOW, addr(1), &bad, &mut out),
            Err(DiscError::BadHash)
        );
        assert_eq!(
            b.handle(NOW, addr(1), &good[..40], &mut out),
            Err(DiscError::Size(40))
        );
        assert_eq!(
            b.handle(NOW, addr(1), &vec![0; 1281], &mut out),
            Err(DiscError::Size(1281))
        );
        // expired a second ago
        assert_eq!(
            b.handle(NOW + 21_000, addr(1), &good, &mut out),
            Err(DiscError::Expired)
        );
        // our own packet
        let mut own = Vec::new();
        let a = net.nodes.get_mut(&addr(1)).unwrap();
        assert_eq!(
            a.handle(NOW, addr(1), &good, &mut own),
            Err(DiscError::SelfPacket)
        );
        assert!(out.is_empty() && own.is_empty());
        // a pong nobody asked for, a neighbors nobody asked for
        let b = net.nodes.get_mut(&addr(2)).unwrap();
        let (pong, _) = encode(
            &key(1),
            &Packet::Pong {
                to: Endpoint::new(addr(2), 0),
                reply_tok: Bytes::from(vec![0; 32]),
                expiration: NOW / 1000 + 20,
            },
        );
        assert_eq!(
            b.handle(NOW, addr(1), &pong, &mut out),
            Err(DiscError::Unsolicited)
        );
        let (nb, _) = encode(
            &key(1),
            &Packet::Neighbors {
                nodes: vec![],
                expiration: NOW / 1000 + 20,
            },
        );
        assert_eq!(
            b.handle(NOW, addr(1), &nb, &mut out),
            Err(DiscError::Unsolicited)
        );
        assert!(b.table().is_empty());
    }

    /// A packet with a signature that recovers some other key, and the ENR packet types, are never
    /// answered; type 1 (a standard ping) is accepted and answered with a pong.
    #[test]
    fn enr_types_ignored_and_standard_ping_accepted() {
        let mut net = Net::new(2);
        let (mut p, _) = encode(
            &key(1),
            &Packet::Ping {
                version: 4,
                from: Endpoint::new(addr(1), 1),
                to: Endpoint::new(addr(2), 0),
                expiration: NOW / 1000 + 20,
            },
        );
        // retype to 1: re-sign by hand is not needed, the hash and signature cover the type byte, so
        // build the datagram with the type changed and re-signed through `encode`'s own steps
        let resign = |p: &mut Vec<u8>, t: u8| {
            p[HEAD] = t;
            let digest = Message::from_digest(keccak256(&p[97..]).0);
            let (rid, sig) = SECP256K1
                .sign_ecdsa_recoverable(&digest, &key(1))
                .serialize_compact();
            p[32..96].copy_from_slice(&sig);
            p[96] = u8::try_from(i32::from(rid)).unwrap();
            let h = keccak256(&p[32..]);
            p[..32].copy_from_slice(h.as_slice());
        };
        let b = net.nodes.get_mut(&addr(2)).unwrap();
        let mut out: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        resign(&mut p, PING);
        b.handle(NOW, addr(1), &p, &mut out).unwrap();
        assert_eq!(out[0].1[HEAD], PONG);
        out.clear();
        resign(&mut p, ENR_RESPONSE);
        assert_eq!(
            b.handle(NOW, addr(1), &p, &mut out),
            Err(DiscError::Unsupported(ENR_RESPONSE))
        );
        for t in [0, 7, 9] {
            resign(&mut p, t);
            assert_eq!(
                b.handle(NOW, addr(1), &p, &mut out),
                Err(DiscError::UnknownType(t))
            );
        }
        assert!(out.is_empty());
    }

    #[test]
    fn table_is_bounded_and_ping_queue_is_bounded() {
        let me = peer_id(&key(1));
        let mut t = Table::new(&me);
        let mut accepted = 0;
        for i in 0..2000u32 {
            let mut id = [0u8; 64];
            id[..4].copy_from_slice(&i.to_be_bytes());
            id[63] = 1;
            let n = Node {
                id: B512::from(id),
                addr: addr(9),
                tcp: 1,
            };
            accepted += usize::from(t.insert(n));
        }
        assert!(t.len() <= BUCKETS * BUCKET_SIZE, "{}", t.len());
        assert_eq!(t.len(), accepted);
        assert_eq!(t.closest(&B256::ZERO, 5).len(), 5);
        assert!(!t.insert(Node {
            id: me,
            addr: addr(1),
            tcp: 1
        }));
        // more pings than MAX_PENDING are not sent
        let mut d = Discv4::new(key(1), addr(1), 1);
        let mut out = Vec::new();
        for i in 0..(u32::try_from(MAX_PENDING).unwrap() + 50) {
            let mut id = [0u8; 64];
            id[..4].copy_from_slice(&i.to_be_bytes());
            d.ping(NOW, addr(2), B512::from(id), &mut out);
        }
        assert_eq!(out.len(), MAX_PENDING);
        d.expire(NOW + RESP_TIMEOUT_MS);
        d.ping(
            NOW + RESP_TIMEOUT_MS,
            addr(2),
            B512::repeat_byte(7),
            &mut out,
        );
        assert_eq!(out.len(), MAX_PENDING + 1);
    }

    #[test]
    fn relay_rules() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let lan = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let wan = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(relay_ok(lo, lo) && relay_ok(lan, lan) && relay_ok(wan, wan));
        assert!(!relay_ok(wan, lo) && !relay_ok(wan, lan));
        assert!(!relay_ok(wan, IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(!relay_ok(wan, IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
    }
}
