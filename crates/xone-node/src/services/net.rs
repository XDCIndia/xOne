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
    /// The advertised enode URL; `--nat any` updates its IP from the discovery pongs (issue #167).
    pub enode: Arc<std::sync::RwLock<String>>,
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
    nat: Nat,
}

/// `--nat` (issue #167): how the advertised address is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nat {
    /// `none`: the listen address (loopback when unspecified), never changed.
    None,
    /// `any` and the default: the IP that at least 3 bonded discovery peers report in their pongs.
    Auto,
    /// `extip:<IP>`: this IP, never changed.
    ExtIp(IpAddr),
}

/// Parses `--nat`; an unknown value, or UPnP and NAT-PMP (no port-mapping client yet), stops the
/// start with an error naming the accepted values.
pub fn parse_nat(v: Option<&str>) -> Result<Nat, String> {
    match v.map(str::trim) {
        None | Some("" | "any") => Ok(Nat::Auto),
        Some("none") => Ok(Nat::None),
        Some(x) if x.starts_with("extip:") => x
            .trim_start_matches("extip:")
            .parse::<IpAddr>()
            .map(Nat::ExtIp)
            .map_err(|_| format!("--nat {x}: not an IP address")),
        Some(x) if x == "upnp" || x == "pmp" || x.starts_with("pmp:") => Err(format!(
            "--nat {x}: UPnP and NAT-PMP are not supported yet; use any, none or extip:<IP>"
        )),
        Some(x) => Err(format!(
            "--nat {x}: unknown value; use any, none or extip:<IP>"
        )),
    }
}

fn enode_url(id: PeerId, host: &str, port: u16) -> String {
    let host = match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(a)) => format!("[{a}]"),
        _ => host.to_owned(),
    };
    format!(
        "enode://{}@{host}:{port}",
        alloy_primitives::hex::encode(id)
    )
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
        let ip: IpAddr = if loopback {
            [127, 0, 0, 1].into()
        } else {
            [0, 0, 0, 0].into()
        };
        let listen = SocketAddr::new(ip, cfg.p2p.port);
        let udp = (!cfg.p2p.no_discover)
            .then(|| SocketAddr::new(ip, cfg.p2p.discovery_port.unwrap_or(cfg.p2p.port)));
        let nat = parse_nat(cfg.p2p.nat.as_deref())?;
        let host = match nat {
            Nat::ExtIp(ip) => ip.to_string(),
            Nat::Auto | Nat::None => "127.0.0.1".to_owned(),
        };
        let id = xone_net::rlpx::peer_id(&key);
        Ok(Self {
            h: NetHandle {
                id,
                enode: Arc::new(std::sync::RwLock::new(enode_url(id, &host, cfg.p2p.port))),
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
            nat,
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
    advertise: Option<(Arc<std::sync::RwLock<String>>, u16)>,
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
                if let Some((enode, port)) = &advertise {
                    if let Some(ip) = d.external_ip(t) {
                        let url = enode_url(d.id(), &ip.to_string(), *port);
                        let mut cur = enode.write().unwrap_or_else(PoisonError::into_inner);
                        if *cur != url {
                            tracing::info!(target: "net", "advertised address now {ip} (discovery)");
                            *cur = url;
                        }
                    }
                }
                d.revalidate(t, &mut sock);
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
                (self.nat == Nat::Auto).then(|| (self.h.enode.clone(), self.listen.port())),
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
mod nat_tests {
    use super::*;

    #[test]
    fn nat_values() {
        assert_eq!(parse_nat(None), Ok(Nat::Auto));
        assert_eq!(parse_nat(Some("any")), Ok(Nat::Auto));
        assert_eq!(parse_nat(Some("none")), Ok(Nat::None));
        assert_eq!(
            parse_nat(Some("extip:203.0.113.7")),
            Ok(Nat::ExtIp([203, 0, 113, 7].into()))
        );
        for bad in [
            "extip:nope",
            "upnp",
            "pmp",
            "pmp:192.168.0.1",
            "stun",
            "ANY",
        ] {
            assert!(parse_nat(Some(bad)).is_err(), "{bad}");
        }
        let id = PeerId::repeat_byte(0xab);
        assert!(enode_url(id, "::1", 30303).ends_with("@[::1]:30303"));
        assert!(enode_url(id, "10.0.0.1", 30303).ends_with("@10.0.0.1:30303"));
    }
}
