//! The `net` service: `xone_net::Peers` on the node's runtime with the TCP accept loop, a redial loop
//! per static, trusted or boot peer, and the discv4 UDP loop (ping, pong and findnode to seed the
//! table, then dial what it finds). The per-peer queues of ADR-007 are those of `Peers`.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{B256, B512};
use secp256k1::SecretKey;
use tokio::{
    net::{TcpListener, UdpSocket},
    runtime::{Handle, Runtime},
    task::JoinHandle,
    time::{interval, sleep},
};
use xone_net::{
    discv4::Discv4,
    peers::{Config as PeersConfig, Peers},
    rlpx::PeerId,
};

use crate::{
    config::Config,
    enode::{read_peer_file, Enode},
    lifecycle::{Service, ServiceError},
};

/// The TCP listener, the discovery socket (None with `--nodiscover`) and the advertised enode of
/// the p2p config. The enode host is the `--nat extip:` address, else a non-wildcard `--p2p.addr`,
/// else 127.0.0.1; it carries `discport` when discovery is on another port (geth `urlv4.go`).
fn endpoints(
    p2p: &crate::config::P2p,
    loopback: bool,
    id_hex: &str,
) -> Result<(SocketAddr, Option<SocketAddr>, Enode), String> {
    let configured = p2p
        .addr
        .as_deref()
        .map(|a| {
            a.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .map_err(|e| format!("--p2p.addr {a}: {e}"))
        })
        .transpose()?;
    let ip: IpAddr = match configured {
        Some(ip) => ip,
        None if loopback => [127, 0, 0, 1].into(),
        None => [0, 0, 0, 0].into(),
    };
    let listen = SocketAddr::new(ip, p2p.port);
    let disc_port = p2p.discovery_port.unwrap_or(p2p.port);
    let udp = (!p2p.no_discover).then(|| SocketAddr::new(ip, disc_port));
    let extip = p2p.nat.as_deref().and_then(|n| n.strip_prefix("extip:"));
    let host = match (extip, configured) {
        (Some(x), _) => x
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .map_err(|e| format!("--nat extip:{x}: {e}"))?
            .to_string(),
        (None, Some(ip)) if !ip.is_unspecified() => ip.to_string(),
        (None, _) => "127.0.0.1".to_owned(),
    };
    let enode = Enode {
        id: id_hex.to_owned(),
        host,
        port: p2p.port,
        discport: udp.filter(|u| u.port() != p2p.port).map(|u| u.port()),
    };
    Ok((listen, udp, enode))
}

/// The node's p2p identity: `<datadir>/nodekey` (hex), created on first start.
pub fn load_or_create_key(datadir: &Path) -> Result<SecretKey, String> {
    let path = datadir.join("nodekey");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let b = alloy_primitives::hex::decode(t.trim())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        return SecretKey::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()));
    }
    let key = loop {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        if let Ok(k) = SecretKey::from_slice(&b) {
            break k;
        }
    };
    std::fs::create_dir_all(datadir).map_err(|e| e.to_string())?;
    std::fs::write(&path, alloy_primitives::hex::encode(key.secret_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key)
}

/// What the other services use of the net service.
#[derive(Clone)]
pub struct NetHandle {
    pub peers: Peers,
    pub id: PeerId,
    pub enode: String,
    pub listen: String,
    pub name: String,
    pub network_id: u64,
    pub genesis: B256,
    rt: Handle,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

fn peer_id(e: &Enode) -> Result<PeerId, String> {
    e.id.parse::<B512>().map_err(|e| e.to_string())
}

async fn resolve(e: &Enode) -> Option<SocketAddr> {
    tokio::net::lookup_host((e.host.as_str(), e.port))
        .await
        .ok()?
        .next()
}

impl NetHandle {
    pub fn runtime(&self) -> Handle {
        self.rt.clone()
    }

    fn spawn(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(self.rt.spawn(f));
    }

    /// Dials `e` now and again every 5 s while it is not connected. A trusted peer also gets the
    /// trust of `xone_net`.
    pub fn add_peer(&self, e: &Enode, trusted: bool) -> Result<(), String> {
        let id = peer_id(e)?;
        if trusted {
            self.peers.add_static(id);
        }
        let (peers, e) = (self.peers.clone(), e.clone());
        self.spawn(async move {
            loop {
                if !peers.peer_ids().contains(&id) {
                    if let Some(addr) = resolve(&e).await {
                        // a failed dial is retried by the loop; the reason is the peer's
                        let _ = peers.dial(addr, id).await;
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
        Ok(())
    }
}

pub struct NetService {
    rt: Option<Runtime>,
    h: NetHandle,
    key: SecretKey,
    listen: SocketAddr,
    udp: Option<SocketAddr>,
    targets: Vec<(Enode, bool)>,
    boots: Vec<Enode>,
    max_peers: usize,
}

impl NetService {
    /// Reads the peer lists of the config and the datadir. `rt` is owned by this service, the first
    /// to start and the last to stop; the others spawn on its handle.
    pub fn new(
        cfg: &Config,
        rt: Runtime,
        peers: Peers,
        key: SecretKey,
        pc: &PeersConfig,
    ) -> Result<Self, String> {
        let mut boots = vec![];
        for b in &cfg.p2p.bootnodes {
            boots.push(
                b.parse::<Enode>()
                    .map_err(|e| format!("--bootnodes: {e}"))?,
            );
        }
        let mut targets: Vec<(Enode, bool)> = boots.iter().map(|b| (b.clone(), false)).collect();
        for (file, trusted) in [("static-nodes.json", true), ("trusted-nodes.json", true)] {
            for e in read_peer_file(&cfg.datadir.join(file)).map_err(|e| e.to_string())? {
                targets.push((e, trusted));
            }
        }
        // a node that only talks to loopback peers and does no discovery has no reason to listen outside
        let loopback = cfg.p2p.no_discover
            && !targets.is_empty()
            && targets
                .iter()
                .all(|(e, _)| e.host.parse::<IpAddr>().is_ok_and(|a| a.is_loopback()));
        let id = xone_net::rlpx::peer_id(&key);
        let (listen, udp, enode) =
            endpoints(&cfg.p2p, loopback, &alloy_primitives::hex::encode(id))?;
        Ok(Self {
            h: NetHandle {
                id,
                enode: enode.to_string(),
                listen: listen.to_string(),
                name: pc.name.clone(),
                network_id: pc.network_id,
                genesis: pc.genesis,
                peers,
                rt: rt.handle().clone(),
                tasks: Arc::default(),
            },
            rt: Some(rt),
            key,
            listen,
            udp,
            targets,
            boots,
            max_peers: cfg.p2p.max_peers as usize,
        })
    }

    pub fn handle(&self) -> NetHandle {
        self.h.clone()
    }
}

async fn accept_loop(l: TcpListener, peers: Peers) {
    loop {
        let Ok((tcp, _)) = l.accept().await else {
            sleep(Duration::from_millis(100)).await;
            continue;
        };
        let peers = peers.clone();
        tokio::spawn(async move {
            // a refused or failed handshake is the peer's; `Peers` has already answered it
            let _ = peers.attach(tcp, None).await;
        });
    }
}

/// discv4: bond with the boot nodes, ask the table for the nodes closest to a random target every 5 s,
/// and dial the nodes it learned (at most once in 30 s each) while below `max_peers`.
async fn discovery(
    sock: UdpSocket,
    mut d: Discv4,
    boots: Vec<(SocketAddr, PeerId)>,
    peers: Peers,
    max: usize,
) {
    let mut sock = sock;
    // Unix milliseconds: discv4 expirations are absolute Unix seconds (geth-XDC
    // p2p/discover/v4_udp.go:269 `time.Now().Add(expiration).Unix()`), not uptime.
    let now = || {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        u64::try_from(ms).unwrap_or(u64::MAX)
    };
    // a fresh socket reports no readiness until the reactor has seen it, and `try_send_to` would drop
    // the first datagrams
    let _ = sock.writable().await;
    for (addr, id) in &boots {
        d.ping(now(), *addr, *id, &mut sock);
    }
    let mut tick = interval(Duration::from_secs(5));
    let mut tried: HashMap<PeerId, u64> = HashMap::new();
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                if let Ok((n, from)) = r {
                    // an invalid or unsolicited packet is dropped, as the protocol says
                    let _ = d.handle(now(), from, &buf[..n], &mut sock);
                }
            }
            _ = tick.tick() => {
                let t = now();
                d.expire(t);
                if d.table().is_empty() {
                    // nobody has answered yet: bond with the boot nodes again
                    for (addr, id) in &boots {
                        d.ping(t, *addr, *id, &mut sock);
                    }
                }
                let mut target = [0u8; 64];
                if getrandom::fill(&mut target).is_ok() {
                    let target = PeerId::from(target);
                    for n in d.table().closest(&alloy_primitives::keccak256(target), 16) {
                        d.find_node(t, &n, target, &mut sock);
                    }
                }
                let connected = peers.peer_ids();
                if connected.len() < max {
                    for n in d.table().closest(&B256::ZERO, 16) {
                        let fresh = tried.get(&n.id).is_none_or(|at| t.saturating_sub(*at) > 30_000);
                        if fresh && !connected.contains(&n.id) {
                            tried.insert(n.id, t);
                            let (p, addr) = (peers.clone(), SocketAddr::new(n.addr.ip(), n.tcp));
                            tokio::spawn(async move {
                                let _ = p.dial(addr, n.id).await;
                            });
                        }
                    }
                }
            }
        }
    }
}

impl Service for NetService {
    fn name(&self) -> &str {
        "net"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let err = |e: String| ServiceError(e);
        let Some(rt) = &self.rt else {
            return Err(err("net service restarted".into()));
        };
        let listener = rt
            .block_on(TcpListener::bind(self.listen))
            .map_err(|e| err(format!("p2p listen {}: {e}", self.listen)))?;
        let peers = self.h.peers.clone();
        self.h.spawn(accept_loop(listener, peers));
        for (e, trusted) in &self.targets {
            self.h.add_peer(e, *trusted).map_err(err)?;
        }
        if let Some(addr) = self.udp {
            let sock = rt
                .block_on(UdpSocket::bind(addr))
                .map_err(|e| err(format!("discovery listen {addr}: {e}")))?;
            let d = Discv4::new(self.key, addr, self.listen.port());
            let mut boots = vec![];
            for b in &self.boots {
                if let Some(a) = rt.block_on(resolve(b)) {
                    boots.push((
                        SocketAddr::new(a.ip(), b.discport.unwrap_or(b.port)),
                        peer_id(b).map_err(err)?,
                    ));
                }
            }
            self.h.spawn(discovery(
                sock,
                d,
                boots,
                self.h.peers.clone(),
                self.max_peers,
            ));
        }
        Ok(())
    }

    fn stop(&mut self) {
        for t in self
            .h
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            t.abort();
        }
        if let Some(rt) = self.rt.take() {
            let peers = self.h.peers.clone();
            // sessions get a Disconnect and are aborted after the grace time of `Peers`
            rt.block_on(async move {
                peers.shutdown().await;
            });
            rt.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::P2p;

    const ID: &str = "8d7c5b2a8b3a0dbfe1d1e4c05b1c4a3b0c1e9d7f2a3b4c5d6e7f8091a2b3c4d58d7c5b2a8b3a0dbfe1d1e4c05b1c4a3b0c1e9d7f2a3b4c5d6e7f8091a2b3c4d5";

    fn p2p(addr: Option<&str>, nat: Option<&str>, disc: Option<u16>) -> P2p {
        P2p {
            addr: addr.map(str::to_owned),
            nat: nat.map(str::to_owned),
            discovery_port: disc,
            ..P2p::default()
        }
    }

    #[test]
    fn ipv4_defaults_are_unchanged() {
        let (tcp, udp, e) = endpoints(&P2p::default(), false, ID).unwrap();
        assert_eq!(tcp, "0.0.0.0:30303".parse().unwrap());
        assert_eq!(udp, Some("0.0.0.0:30303".parse().unwrap()));
        assert_eq!(e.to_string(), format!("enode://{ID}@127.0.0.1:30303"));
        let (tcp, _, _) = endpoints(&P2p::default(), true, ID).unwrap();
        assert_eq!(tcp, "127.0.0.1:30303".parse().unwrap());
        // separate TCP and UDP ports: the enode says so with discport
        let (tcp, udp, e) =
            endpoints(&p2p(None, Some("extip:1.2.3.4"), Some(30301)), false, ID).unwrap();
        assert_eq!((tcp.port(), udp.map(|u| u.port())), (30303, Some(30301)));
        assert_eq!(
            e.to_string(),
            format!("enode://{ID}@1.2.3.4:30303?discport=30301")
        );
        let nodisc = P2p {
            no_discover: true,
            ..P2p::default()
        };
        assert_eq!(endpoints(&nodisc, false, ID).unwrap().1, None);
    }

    #[test]
    fn ipv6_listeners_advertise_bracketed_enodes_that_parse_back() {
        let (tcp, udp, e) = endpoints(&p2p(Some("::1"), None, Some(30301)), false, ID).unwrap();
        assert_eq!(tcp, "[::1]:30303".parse().unwrap());
        assert_eq!(udp, Some("[::1]:30301".parse().unwrap()));
        let url = e.to_string();
        assert_eq!(url, format!("enode://{ID}@[::1]:30303?discport=30301"));
        assert_eq!(url.parse::<Enode>().unwrap(), e);
        // a wildcard listener advertises the extip, IPv6 too
        let (tcp, _, e) =
            endpoints(&p2p(Some("::"), Some("extip:2001:db8::7"), None), false, ID).unwrap();
        assert_eq!(tcp, "[::]:30303".parse().unwrap());
        assert_eq!(e.to_string(), format!("enode://{ID}@[2001:db8::7]:30303"));
        assert_eq!(e.to_string().parse::<Enode>().unwrap(), e);
        assert!(endpoints(&p2p(Some("nope"), None, None), false, ID).is_err());
        assert!(endpoints(&p2p(None, Some("extip:nope"), None), false, ID).is_err());
    }
}
