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
    pub enode: String,
    pub listen: String,
    pub name: String,
    pub network_id: u64,
    pub genesis: B256,
    rt: Handle,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// One redial job per peer identity (#296): the endpoint it dials and its task.
    dials: Arc<Mutex<Dials>>,
}

type Dials = HashMap<PeerId, (Enode, JoinHandle<()>)>;

/// At most this many peers are redialled; `admin_addPeer` beyond it is refused.
const MAX_DIALS: usize = 1024;
/// Redial backoff: from the first to the last delay, doubling after each failed round, with up to
/// a quarter of jitter so that peers dropped together do not come back in step.
const DIAL_FIRST: Duration = Duration::from_secs(5);
const DIAL_LAST: Duration = Duration::from_secs(300);

/// The delay after `fails` failed rounds, `jitter` in 0..=255 scaling up to +25 %.
fn dial_backoff(fails: u32, jitter: u8) -> Duration {
    let base = DIAL_FIRST
        .saturating_mul(1u32 << fails.min(16))
        .min(DIAL_LAST);
    base + base / 4 * u32::from(jitter) / 255
}

fn jitter() -> u8 {
    let mut b = [0u8; 1];
    let _ = getrandom::fill(&mut b);
    b[0]
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

    /// Dials `e` now and again while it is not connected, with a bounded, jittered backoff after
    /// failures. One job per peer identity: adding the same peer again changes nothing, adding it
    /// with another endpoint replaces the job. A trusted peer also gets the trust of `xone_net`.
    pub fn add_peer(&self, e: &Enode, trusted: bool) -> Result<(), String> {
        let id = peer_id(e)?;
        let mut dials = self.dials.lock().unwrap_or_else(PoisonError::into_inner);
        dials.retain(|_, (_, t)| !t.is_finished());
        let known = dials.get(&id).map(|(old, _)| old == e);
        if known.is_none() && dials.len() >= MAX_DIALS {
            return Err(format!("too many dialled peers (at most {MAX_DIALS})"));
        }
        if trusted {
            self.peers.add_static(id);
        }
        if known == Some(true) {
            return Ok(());
        }
        let (peers, target) = (self.peers.clone(), e.clone());
        let job = self.rt.spawn(async move {
            let mut fails = 0u32;
            loop {
                if peers.peer_ids().contains(&id) {
                    fails = 0;
                } else {
                    let ok = match resolve(&target).await {
                        // a failed dial is retried; the reason is the peer's
                        Some(addr) => peers.dial(addr, id).await.is_ok(),
                        None => false,
                    };
                    fails = if ok { 0 } else { fails.saturating_add(1) };
                }
                sleep(dial_backoff(fails.saturating_sub(1), jitter())).await;
            }
        });
        if let Some((_, old)) = dials.insert(id, (e.clone(), job)) {
            old.abort();
        }
        Ok(())
    }

    /// Stops redialling `id`. True if a job was running.
    pub fn remove_dial(&self, id: &PeerId) -> bool {
        let job = self
            .dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        job.map(|(_, t)| t.abort()).is_some()
    }

    fn abort_dials(&self) {
        for (_, (_, t)) in self
            .dials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            t.abort();
        }
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
        let ip: IpAddr = if loopback {
            [127, 0, 0, 1].into()
        } else {
            [0, 0, 0, 0].into()
        };
        let listen = SocketAddr::new(ip, cfg.p2p.port);
        let udp = (!cfg.p2p.no_discover)
            .then(|| SocketAddr::new(ip, cfg.p2p.discovery_port.unwrap_or(cfg.p2p.port)));
        let host = match cfg
            .p2p
            .nat
            .as_deref()
            .and_then(|n| n.strip_prefix("extip:"))
        {
            Some(x) => x.to_owned(),
            None => "127.0.0.1".to_owned(),
        };
        let id = xone_net::rlpx::peer_id(&key);
        Ok(Self {
            h: NetHandle {
                id,
                enode: format!(
                    "enode://{}@{host}:{}",
                    alloy_primitives::hex::encode(id),
                    cfg.p2p.port
                ),
                listen: listen.to_string(),
                name: pc.name.clone(),
                network_id: pc.network_id,
                genesis: pc.genesis,
                peers,
                rt: rt.handle().clone(),
                tasks: Arc::default(),
                dials: Arc::default(),
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
        self.h.abort_dials();
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
mod dial_tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_ceiling_with_bounded_jitter() {
        assert_eq!(dial_backoff(0, 0), DIAL_FIRST);
        assert_eq!(dial_backoff(1, 0), DIAL_FIRST * 2);
        assert_eq!(dial_backoff(40, 0), DIAL_LAST);
        assert_eq!(dial_backoff(40, 255), DIAL_LAST + DIAL_LAST / 4);
        assert!(dial_backoff(0, 128) > DIAL_FIRST && dial_backoff(0, 128) < DIAL_FIRST * 5 / 4);
    }
}
