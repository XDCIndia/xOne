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
/// How long a pong keeps proving an endpoint (geth `v4_udp.go` `bondExpiration`). An older bond
/// no longer authorizes a FindNode reply, and a ping from that node is answered with a new ping.
pub const BOND_EXPIRATION_MS: u64 = 24 * 60 * 60 * 1000;
/// A table entry not proven for this long is due for revalidation (one probe per call of
/// [`Discv4::revalidate`]).
pub const REVALIDATE_MS: u64 = 30_000;
/// Unanswered revalidation pings after which an entry is dropped for a replacement.
pub const MAX_FAILS: u8 = 3;
/// Verified nodes kept per bucket for when a member dies (geth `table.go` `maxReplacements`).
pub const MAX_REPLACEMENTS: usize = 10;
/// Public entries one subnet ([`subnet`]) may hold in one bucket and in the whole table (geth
/// `p2p/discover/table.go` `bucketIPLimit`, `tableIPLimit`).
pub const BUCKET_SUBNET_LIMIT: usize = 2;
pub const TABLE_SUBNET_LIMIT: usize = 10;
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
    /// The IP of the endpoint: 4 or 16 bytes, else `None`.
    pub fn ip_addr(&self) -> Option<IpAddr> {
        match self.ip.len() {
            4 => <[u8; 4]>::try_from(self.ip.as_ref()).ok().map(IpAddr::from),
            16 => <[u8; 16]>::try_from(self.ip.as_ref())
                .ok()
                .map(IpAddr::from),
            _ => None,
        }
    }

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

#[derive(Debug, Clone)]
struct Entry {
    node: Node,
    /// When a pong last proved `node.addr` (Unix ms).
    bond_ms: u64,
    /// Revalidation pings unanswered since that pong.
    fails: u8,
}

/// The diversity group of a public address: its /24 for IPv4, its /48 for IPv6. `None` for a
/// loopback, private, link-local or unique-local address: geth's `DistinctNetSet.Add` admits
/// LAN addresses without counting them, so a private network can bootstrap from one subnet. The
/// exception grants no trust: such a node still needs a pong and is only relayed to LAN hosts.
/// IPv6 uses /48 (an end-site allocation) where geth applies its 24-bit mask to both families.
fn subnet(ip: IpAddr) -> Option<[u8; 7]> {
    match ip {
        IpAddr::V4(a) if a.is_loopback() || a.is_private() || a.is_link_local() => None,
        IpAddr::V4(a) => {
            let o = a.octets();
            Some([4, o[0], o[1], o[2], 0, 0, 0])
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return subnet(IpAddr::V4(v4));
            }
            let seg = a.segments();
            if a.is_loopback() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80 {
                return None;
            }
            let o = a.octets();
            Some([6, o[0], o[1], o[2], o[3], o[4], o[5]])
        }
    }
}

/// Nodes by log distance from us: 17 buckets of at most 16, so at most 272 nodes. A node is in
/// the table only after a pong proved its endpoint. A full bucket keeps its members and puts the
/// new node in that bucket's replacement list (at most [`MAX_REPLACEMENTS`], newest kept). A
/// member whose endpoint fails [`MAX_FAILS`] revalidation pings in a row is dropped and the
/// newest replacement that fits the subnet limits takes its place (geth `table.go`
/// `addReplacement`, `table_reval.go`). One public subnet holds at most [`BUCKET_SUBNET_LIMIT`]
/// entries of a bucket and [`TABLE_SUBNET_LIMIT`] of the table; the counts are derived from the
/// entries, so insert, refresh and an endpoint change can never leave them stale.
#[derive(Debug)]
pub struct Table {
    local: B256,
    buckets: [Vec<Entry>; BUCKETS],
    repl: [Vec<Entry>; BUCKETS],
}

impl Table {
    pub fn new(local: &PeerId) -> Self {
        Self {
            local: keccak256(local),
            buckets: Default::default(),
            repl: Default::default(),
        }
    }

    fn bucket(&self, id: &B256) -> Option<usize> {
        match log_dist(&self.local, id) {
            0 => None,
            d => Some(d.saturating_sub(MIN_DISTANCE).min(BUCKETS - 1)),
        }
    }

    /// Adds or refreshes a node whose endpoint a pong proved at `now_ms`. `false`: the node is us,
    /// its subnet already has its share of the bucket or table (a refused endpoint change keeps the
    /// old entry as it is), or its bucket is full (it is then kept as a replacement).
    pub fn insert(&mut self, node: Node, now_ms: u64) -> bool {
        let Some(b) = self.bucket(&keccak256(node.id)) else {
            return false;
        };
        if !self.subnet_ok(b, &node) {
            return false;
        }
        let entry = Entry {
            node,
            bond_ms: now_ms,
            fails: 0,
        };
        let bucket = &mut self.buckets[b];
        if let Some(e) = bucket.iter_mut().find(|e| e.node.id == entry.node.id) {
            *e = entry;
            return true;
        }
        if bucket.len() >= BUCKET_SIZE {
            let r = &mut self.repl[b];
            r.retain(|e| e.node.id != entry.node.id);
            if r.len() >= MAX_REPLACEMENTS {
                r.remove(0);
            }
            r.push(entry);
            return false;
        }
        bucket.push(entry);
        true
    }

    /// Whether `node` fits the subnet limits of bucket `b` and of the table; `node` itself is not
    /// counted, so a refresh does not compete with itself.
    fn subnet_ok(&self, b: usize, node: &Node) -> bool {
        let Some(net) = subnet(node.addr.ip()) else {
            return true;
        };
        let same = |e: &&Entry| e.node.id != node.id && subnet(e.node.addr.ip()) == Some(net);
        self.buckets[b].iter().filter(same).count() < BUCKET_SUBNET_LIMIT
            && self.buckets.iter().flatten().filter(same).count() < TABLE_SUBNET_LIMIT
    }

    /// A member's endpoint did not answer a ping. After [`MAX_FAILS`] in a row it is removed and
    /// the newest replacement of its bucket, if any, takes the place. `true`: it was removed.
    pub fn fail(&mut self, id: &PeerId) -> bool {
        let Some(b) = self.bucket(&keccak256(id)) else {
            return false;
        };
        let bucket = &mut self.buckets[b];
        let Some(i) = bucket.iter().position(|e| e.node.id == *id) else {
            return false;
        };
        let e = &mut bucket[i];
        e.fails = e.fails.saturating_add(1);
        if e.fails < MAX_FAILS {
            return false;
        }
        bucket.remove(i);
        while let Some(r) = self.repl[b].pop() {
            if self.subnet_ok(b, &r.node) {
                self.buckets[b].push(r);
                break;
            }
        }
        true
    }

    /// The member proven longest ago, if that was [`REVALIDATE_MS`] or more before `now_ms`.
    pub fn stalest(&self, now_ms: u64) -> Option<Node> {
        self.buckets
            .iter()
            .flatten()
            .filter(|e| now_ms.saturating_sub(e.bond_ms) >= REVALIDATE_MS)
            .min_by_key(|e| e.bond_ms)
            .map(|e| e.node.clone())
    }

    /// The member, if a pong proved its endpoint within [`BOND_EXPIRATION_MS`] of `now_ms`.
    pub fn bonded(&self, id: &PeerId, now_ms: u64) -> Option<&Node> {
        self.entry(id)
            .filter(|e| now_ms.saturating_sub(e.bond_ms) < BOND_EXPIRATION_MS)
            .map(|e| &e.node)
    }

    fn entry(&self, id: &PeerId) -> Option<&Entry> {
        self.buckets.iter().flatten().find(|e| e.node.id == *id)
    }

    pub fn len(&self) -> usize {
        self.buckets.iter().map(Vec::len).sum()
    }

    /// Replacement candidates held, at most `17 * MAX_REPLACEMENTS`.
    pub fn replacements(&self) -> usize {
        self.repl.iter().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &PeerId) -> Option<&Node> {
        self.entry(id).map(|e| &e.node)
    }

    /// The `n` nodes closest to `target` (a keccak of a node id), nearest first.
    pub fn closest(&self, target: &B256, n: usize) -> Vec<Node> {
        let mut all: Vec<(B256, &Node)> = self
            .buckets
            .iter()
            .flatten()
            .map(|e| &e.node)
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

/// How long a pong's statement of our address counts (geth `p2p/enode` `iptrackWindow`).
pub const IP_WINDOW_MS: u64 = 5 * 60 * 1_000;
/// Distinct reporters that must agree before an address is predicted (issue #167).
pub const IP_MIN_STATEMENTS: usize = 3;

/// Our external IP as the pongs of other nodes state it (geth `p2p/netutil/iptrack.go`): the
/// latest statement per reporter within [`IP_WINDOW_MS`]; an IP stated by at least
/// [`IP_MIN_STATEMENTS`] distinct reporters, the most stated one, is the prediction. Reporters on a
/// LAN or loopback address do not count for a public address.
#[derive(Debug, Default)]
pub struct IpTracker {
    statements: HashMap<PeerId, (IpAddr, u64)>,
}

impl IpTracker {
    /// Bounds the map: the oldest statement goes first.
    const MAX: usize = 1_024;

    pub fn add(&mut self, reporter: PeerId, ip: IpAddr, now_ms: u64) {
        if ip.is_unspecified() || ip.is_multicast() {
            return;
        }
        if self.statements.len() >= Self::MAX && !self.statements.contains_key(&reporter) {
            if let Some(old) = self
                .statements
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| *k)
            {
                self.statements.remove(&old);
            }
        }
        self.statements.insert(reporter, (ip, now_ms));
    }

    pub fn predict(&mut self, now_ms: u64) -> Option<IpAddr> {
        self.statements
            .retain(|_, (_, t)| now_ms.saturating_sub(*t) < IP_WINDOW_MS);
        let mut counts: HashMap<IpAddr, usize> = HashMap::new();
        for (ip, _) in self.statements.values() {
            *counts.entry(*ip).or_default() += 1;
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n >= IP_MIN_STATEMENTS)
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(ip, _)| ip)
    }
}

pub struct Discv4 {
    key: SecretKey,
    id: PeerId,
    local: SocketAddr,
    tcp_port: u16,
    table: Table,
    pings: HashMap<PeerId, PendingPing>,
    finds: HashMap<PeerId, PendingFind>,
    ips: IpTracker,
}

impl Discv4 {
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
            ips: IpTracker::default(),
        }
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Our external IP as at least [`IP_MIN_STATEMENTS`] bonded nodes see it (issue #167).
    pub fn external_ip(&mut self, now_ms: u64) -> Option<IpAddr> {
        self.ips.predict(now_ms)
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

    /// Forget requests older than [`RESP_TIMEOUT_MS`]. An unanswered ping to a table member's
    /// recorded endpoint counts as a failure of that member ([`Table::fail`]).
    pub fn expire(&mut self, now_ms: u64) {
        let table = &mut self.table;
        self.pings.retain(|id, p| {
            let live = now_ms.saturating_sub(p.sent_ms) < RESP_TIMEOUT_MS;
            if !live && table.get(id).is_some_and(|n| n.addr == p.addr) {
                table.fail(id);
            }
            live
        });
        self.finds
            .retain(|_, f| now_ms.saturating_sub(f.sent_ms) < RESP_TIMEOUT_MS);
    }

    /// Probe the member proven longest ago, when it is due ([`REVALIDATE_MS`]). Call it on a
    /// timer after [`Self::expire`]: one ping per call bounds the refresh traffic, and a member
    /// that stays silent is replaced after [`MAX_FAILS`] calls.
    pub fn revalidate(&mut self, now_ms: u64, t: &mut impl Transport) {
        if let Some(n) = self.table.stalest(now_ms) {
            if !self.pings.contains_key(&n.id) {
                self.ping_with_tcp(now_ms, n.addr, n.id, n.tcp, t);
            }
        }
    }

    /// Process one datagram. An `Err` is a dropped packet; nothing is sent for it.
    pub fn handle(
        &mut self,
        now_ms: u64,
        from: SocketAddr,
        data: &[u8],
        t: &mut impl Transport,
    ) -> Result<(), DiscError> {
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
                // Bond back unless a current bond proves this endpoint (`v4_udp.go:704-711`).
                if self
                    .table
                    .bonded(&id, now_ms)
                    .is_none_or(|n| n.addr != from)
                {
                    self.ping_with_tcp(now_ms, from, id, ep.tcp, t);
                }
                Ok(())
            }
            Packet::Pong {
                to,
                reply_tok,
                expiration,
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
                self.table.insert(node, now_ms);
                // a verified pong: its `to` is our address as this node sees it
                if let Some(ip) = to.ip_addr() {
                    if relay_ok(from.ip(), ip) {
                        self.ips.add(id, ip, now_ms);
                    }
                }
                Ok(())
            }
            Packet::FindNode { target, expiration } => {
                if expired(expiration, now_ms) {
                    return Err(DiscError::Expired);
                }
                // No endpoint proof, no answer: a findnode from a spoofed address must not make
                // us send a larger packet to the victim (`v4_udp.go:735-752`). An expired bond
                // must be renewed by a ping/pong first.
                if self
                    .table
                    .bonded(&id, now_ms)
                    .is_none_or(|n| n.addr.ip() != from.ip())
                {
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
            accepted += usize::from(t.insert(n, NOW));
        }
        assert!(t.len() <= BUCKETS * BUCKET_SIZE, "{}", t.len());
        assert_eq!(t.len(), accepted);
        assert_eq!(t.closest(&B256::ZERO, 5).len(), 5);
        assert!(!t.insert(
            Node {
                id: me,
                addr: addr(1),
                tcp: 1
            },
            NOW
        ));
        assert!(t.replacements() <= BUCKETS * MAX_REPLACEMENTS);
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

    fn far(i: u32) -> Node {
        let mut id = [0u8; 64];
        id[..4].copy_from_slice(&i.to_be_bytes());
        id[63] = 1;
        Node {
            id: B512::from(id),
            addr: addr(9),
            tcp: 1,
        }
    }

    /// Fills one bucket and returns (members, others in the same bucket).
    fn full_bucket(t: &mut Table) -> (Vec<Node>, Vec<Node>) {
        let b0 = t.bucket(&keccak256(far(0).id));
        let same: Vec<Node> = (0..5000u32)
            .map(far)
            .filter(|n| t.bucket(&keccak256(n.id)) == b0)
            .collect();
        let (m, o) = same.split_at(BUCKET_SIZE);
        for n in m {
            assert!(t.insert(n.clone(), NOW));
        }
        (m.to_vec(), o.to_vec())
    }

    #[test]
    fn full_bucket_with_a_dead_member_admits_a_verified_replacement() {
        let mut t = Table::new(&peer_id(&key(1)));
        let (members, others) = full_bucket(&mut t);
        // a full bucket refuses, but keeps a bounded list of replacements
        for n in others.iter().take(MAX_REPLACEMENTS + 5) {
            assert!(!t.insert(n.clone(), NOW));
        }
        assert_eq!(t.replacements(), MAX_REPLACEMENTS);
        let newest = &others[MAX_REPLACEMENTS + 4];
        // the dead member survives MAX_FAILS - 1 failures, then the newest replacement steps in
        for _ in 1..MAX_FAILS {
            assert!(!t.fail(&members[0].id));
        }
        assert!(t.get(&members[0].id).is_some());
        assert!(t.fail(&members[0].id));
        assert!(t.get(&members[0].id).is_none());
        assert!(t.get(&newest.id).is_some());
        assert_eq!(t.len(), BUCKET_SIZE);
        assert_eq!(t.replacements(), MAX_REPLACEMENTS - 1);
        // a pong resets the failure count
        t.fail(&members[1].id);
        t.insert(members[1].clone(), NOW + 1);
        for _ in 1..MAX_FAILS {
            assert!(!t.fail(&members[1].id));
        }
    }

    #[test]
    fn revalidation_is_one_probe_per_tick_and_drops_silent_nodes() {
        let mut d = Discv4::new(key(1), addr(1), 1);
        let (members, _) = full_bucket(&mut d.table);
        let mut out = Vec::new();
        // nothing is due before REVALIDATE_MS
        d.revalidate(NOW + REVALIDATE_MS - 1, &mut out);
        assert!(out.is_empty());
        // each tick: the earlier probe times out, one new probe is sent
        let mut t = NOW + REVALIDATE_MS;
        let ticks = usize::from(MAX_FAILS) * members.len();
        for _ in 0..ticks {
            d.expire(t);
            d.revalidate(t, &mut out);
            t += 5_000;
        }
        d.expire(t);
        assert_eq!(out.len(), ticks, "one ping per tick");
        assert!(d.pings.len() <= 1);
        // nobody answered: the bucket emptied after MAX_FAILS probes each
        assert!(d.table.is_empty(), "{}", d.table.len());
    }

    #[test]
    fn stale_bond_does_not_authorize_findnode() {
        let mut net = Net::new(2);
        net.ping(1, 2);
        net.run();
        let id2 = net.id(2);
        let later = NOW + BOND_EXPIRATION_MS;
        let a = &net.nodes[&addr(1)];
        assert!(a.table().bonded(&id2, NOW).is_some());
        assert!(a.table().bonded(&id2, later).is_none());
        // node 2 asks node 1 after the bond expired: no neighbors are sent
        let (req, _) = encode(
            &key(2),
            &Packet::FindNode {
                target: id2,
                expiration: later / 1000 + EXPIRATION_S,
            },
        );
        let mut out = Vec::new();
        let d = net.nodes.get_mut(&addr(1)).unwrap();
        assert!(matches!(
            d.handle(later, addr(2), &req, &mut out),
            Err(DiscError::UnknownNode)
        ));
        assert!(out.is_empty());
        // a ping from it is answered with a pong and a new bonding ping
        let (ping, _) = encode(
            &key(2),
            &Packet::Ping {
                version: 4,
                from: Endpoint::new(addr(2), 30_002),
                to: Endpoint::new(addr(1), 0),
                expiration: later / 1000 + EXPIRATION_S,
            },
        );
        d.handle(later, addr(2), &ping, &mut out).unwrap();
        assert_eq!(out.len(), 2);
    }

    fn pub_node(i: u32, ip: [u8; 4]) -> Node {
        let mut id = [0u8; 64];
        id[..4].copy_from_slice(&i.to_be_bytes());
        id[63] = 1;
        Node {
            id: B512::from(id),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 30_303),
            tcp: 30_303,
        }
    }

    #[test]
    fn one_subnet_cannot_fill_the_table() {
        let me = peer_id(&key(1));
        let mut t = Table::new(&me);
        for i in 0..2000u32 {
            t.insert(pub_node(i, [8, 8, 8, 1]), NOW);
        }
        assert_eq!(t.len(), TABLE_SUBNET_LIMIT);
        for b in &t.buckets {
            assert!(b.len() <= BUCKET_SUBNET_LIMIT);
        }
        // another address in the same /24 counts against the same group
        assert!(!(2000..3000u32).any(|i| t.insert(pub_node(i, [8, 8, 8, 200]), NOW)));
        // diverse endpoints are still admitted
        let diverse = (0..200u32)
            .filter(|i| {
                t.insert(
                    pub_node(10_000 + i, [9, u8::try_from(*i).unwrap(), 0, 1]),
                    NOW,
                )
            })
            .count();
        // (bounded only by bucket capacity: the ids crowd into the farthest buckets)
        assert!(diverse >= 40, "{diverse}");
        assert!(t.len() <= BUCKETS * BUCKET_SIZE);
    }

    #[test]
    fn refresh_and_endpoint_change_keep_subnet_counts() {
        let me = peer_id(&key(1));
        let mut t = Table::new(&me);
        let ins: Vec<Node> = (0..2000u32)
            .map(|i| pub_node(i, [8, 8, 8, 1]))
            .filter(|n| t.insert(n.clone(), NOW))
            .collect();
        assert_eq!(ins.len(), TABLE_SUBNET_LIMIT);
        // re-inserting a member (same or a new address in its subnet) is a refresh, not a new entry
        assert!(t.insert(ins[0].clone(), NOW));
        let mut moved = ins[0].clone();
        moved.addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 9)), 30_303);
        assert!(t.insert(moved.clone(), NOW));
        assert_eq!(t.get(&moved.id).map(|n| n.addr), Some(moved.addr));
        assert_eq!(t.len(), TABLE_SUBNET_LIMIT);
        // moving out of the subnet frees a slot for a new member
        moved.addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 30_303);
        assert!(t.insert(moved, NOW));
        assert!((5000..7000u32).any(|i| t.insert(pub_node(i, [8, 8, 8, 1]), NOW)));
        // a member of a full subnet cannot move into another full subnet; it stays as it was
        let full: Vec<Node> = (7000..9000u32)
            .map(|i| pub_node(i, [7, 7, 7, 1]))
            .filter(|n| t.insert(n.clone(), NOW))
            .collect();
        assert_eq!(full.len(), TABLE_SUBNET_LIMIT);
        let mut jump = ins[1].clone();
        jump.addr = full[0].addr;
        assert!(!t.insert(jump, NOW));
        assert_eq!(t.get(&ins[1].id).map(|n| n.addr), Some(ins[1].addr));
    }

    #[test]
    fn lan_addresses_are_exempt_and_v6_groups_by_48() {
        assert_eq!(subnet(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))), None);
        assert_eq!(subnet(IpAddr::V4(Ipv4Addr::LOCALHOST)), None);
        assert_eq!(subnet("fd00::1".parse().unwrap()), None);
        assert_eq!(subnet("fe80::1".parse().unwrap()), None);
        assert_eq!(
            subnet("::ffff:8.8.8.8".parse().unwrap()),
            subnet(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 9)))
        );
        assert_eq!(
            subnet("2001:db8:1::1".parse().unwrap()),
            subnet("2001:db8:1:ffff::2".parse().unwrap())
        );
        assert_ne!(
            subnet("2001:db8:1::1".parse().unwrap()),
            subnet("2001:db8:2::1".parse().unwrap())
        );
    }

    #[test]
    fn ip_tracker_needs_three_agreeing_reporters_in_the_window() {
        let mut t = IpTracker::default();
        let ext = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let other = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
        t.add(PeerId::repeat_byte(1), ext, NOW);
        t.add(PeerId::repeat_byte(2), ext, NOW);
        // the same reporter twice is one statement
        t.add(PeerId::repeat_byte(2), ext, NOW);
        t.add(PeerId::repeat_byte(3), other, NOW);
        assert_eq!(t.predict(NOW), None);
        t.add(PeerId::repeat_byte(4), ext, NOW);
        assert_eq!(t.predict(NOW), Some(ext));
        // a reporter that changes its statement moves its vote
        t.add(PeerId::repeat_byte(4), other, NOW + 1);
        assert_eq!(t.predict(NOW + 1), None);
        t.add(PeerId::repeat_byte(5), ext, NOW + 1);
        assert_eq!(t.predict(NOW + 1), Some(ext));
        // statements expire
        assert_eq!(t.predict(NOW + IP_WINDOW_MS), None);
        t.add(
            PeerId::repeat_byte(6),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            NOW,
        );
        assert!(!t.statements.contains_key(&PeerId::repeat_byte(6)));
    }

    #[test]
    fn verified_pongs_feed_the_external_ip() {
        let mut net = Net::new(4);
        for b in 2..=4 {
            net.ping(1, b);
        }
        net.run();
        let me = net.nodes.get_mut(&addr(1)).unwrap();
        assert_eq!(me.external_ip(NOW), Some(addr(1).ip()));
        // node 2 has heard from one node only
        assert_eq!(net.nodes.get_mut(&addr(2)).unwrap().external_ip(NOW), None);
    }
}
