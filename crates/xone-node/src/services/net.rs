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

use super::peerfile::{PeerFile, SAVE_EVERY_SECS};
use crate::{
    config::Config,
    enode::{read_peer_file, Enode},
    lifecycle::{Service, ServiceError},
};

type SharedPeerFile = Option<Arc<Mutex<PeerFile>>>;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Records a completed outbound handshake in the peer table.
fn remember(pf: &SharedPeerFile, id: PeerId, addr: SocketAddr) {
    if let Some(pf) = pf {
        let e = Enode {
            id: alloy_primitives::hex::encode(id),
            host: addr.ip().to_string(),
            port: addr.port(),
            discport: None,
        };
        pf.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(&e, unix_now());
    }
}

fn save(pf: &SharedPeerFile) {
    if let Some(pf) = pf {
        let mut g = pf.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(e) = g.save(unix_now()) {
            eprintln!("net: peer table not written: {e}");
        }
    }
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
    /// The persistent peer table (`peerfile.rs`); None with `--peers.nopersist`.
    peer_file: SharedPeerFile,
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
        let (peers, e, pf) = (self.peers.clone(), e.clone(), self.peer_file.clone());
        self.spawn(async move {
            loop {
                if !peers.peer_ids().contains(&id) {
                    if let Some(addr) = resolve(&e).await {
                        // a failed dial is retried by the loop; the reason is the peer's
                        if peers.dial(addr, id).await.is_ok() {
                            remember(&pf, id, addr);
                        }
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
        Ok(())
    }

    /// Dials `e` once (a peer of the persistent table); a success refreshes its entry.
    fn dial_once(&self, e: &Enode) -> Result<(), String> {
        let id = peer_id(e)?;
        let (peers, e, pf) = (self.peers.clone(), e.clone(), self.peer_file.clone());
        self.spawn(async move {
            if peers.peer_ids().contains(&id) {
                return;
            }
            if let Some(addr) = resolve(&e).await {
                if peers.dial(addr, id).await.is_ok() {
                    remember(&pf, id, addr);
                }
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
        let peer_file = (!cfg.p2p.peers_nopersist).then(|| {
            let path = cfg
                .p2p
                .peers_file
                .clone()
                .unwrap_or_else(|| cfg.datadir.join("peers.json"));
            let (pf, note) = PeerFile::load(&path, unix_now());
            if let Some(n) = note {
                eprintln!("net: {n}");
            }
            Arc::new(Mutex::new(pf))
        });
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
                peer_file,
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
    pf: SharedPeerFile,
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
                            let pf = pf.clone();
                            tokio::spawn(async move {
                                if p.dial(addr, n.id).await.is_ok() {
                                    remember(&pf, n.id, addr);
                                }
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
        // after the static and trusted peers: the most recent peers of the persistent table
        let known: Vec<Enode> = self.h.peer_file.as_ref().map_or_else(Vec::new, |pf| {
            pf.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .best(self.max_peers)
        });
        let targeted: Vec<&str> = self.targets.iter().map(|(e, _)| e.id.as_str()).collect();
        let known: Vec<Enode> = known
            .into_iter()
            .filter(|e| !targeted.contains(&e.id.as_str()))
            .collect();
        for e in &known {
            self.h.dial_once(e).map_err(err)?;
        }
        if self.h.peer_file.is_some() {
            let pf = self.h.peer_file.clone();
            self.h.spawn(async move {
                let mut t = interval(Duration::from_secs(SAVE_EVERY_SECS));
                t.tick().await;
                loop {
                    t.tick().await;
                    save(&pf);
                }
            });
        }
        if let Some(addr) = self.udp {
            let sock = rt
                .block_on(UdpSocket::bind(addr))
                .map_err(|e| err(format!("discovery listen {addr}: {e}")))?;
            let d = Discv4::new(self.key, addr, self.listen.port());
            let mut boots = vec![];
            // the boot nodes, and the known peers to seed the discovery table
            for b in self.boots.iter().chain(&known) {
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
                self.h.peer_file.clone(),
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
        save(&self.h.peer_file);
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

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_or(0, |a| a.port())
    }

    fn key(b: u8) -> SecretKey {
        SecretKey::from_slice(&[b; 32])
            .unwrap_or_else(|_| SecretKey::from_slice(&[1; 32]).unwrap_or_else(|_| unreachable!()))
    }

    struct Node {
        svc: NetService,
        _events: tokio::sync::mpsc::Receiver<xone_net::peers::Event>,
    }

    fn node(dir: &Path, port: u16, k: SecretKey) -> Node {
        let mut cfg = Config {
            datadir: dir.to_owned(),
            ..Config::default()
        };
        cfg.p2p.port = port;
        cfg.p2p.no_discover = true;
        cfg.p2p.nat = Some("extip:127.0.0.1".into());
        let pc = PeersConfig::new("xone/test", 551, B256::ZERO);
        let (peers, events) = Peers::new(pc.clone(), k);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("runtime: {e}"));
        let mut svc = NetService::new(&cfg, rt, peers, k, &pc).unwrap_or_else(|e| panic!("{e}"));
        svc.start().unwrap_or_else(|e| panic!("{}", e.0));
        Node {
            svc,
            _events: events,
        }
    }

    fn wait_connected(n: &Node, to: PeerId) -> bool {
        for _ in 0..100 {
            if n.svc.h.peers.peer_ids().contains(&to) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_restart_with_no_bootnodes_reconnects_from_the_peer_table() {
        let root = std::env::temp_dir().join(format!("xone-peertable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (da, db) = (root.join("a"), root.join("b"));
        let _ = std::fs::create_dir_all(&da);
        let _ = std::fs::create_dir_all(&db);
        let (ka, kb) = (key(0x11), key(0x22));
        let (pa, pb) = (free_port(), free_port());
        let b = node(&db, pb, kb);
        let idb = xone_net::rlpx::peer_id(&kb);
        // first start: B is a static peer of A
        let static_nodes = da.join("static-nodes.json");
        let _ = std::fs::write(
            &static_nodes,
            format!(
                "[\"enode://{}@127.0.0.1:{pb}\"]",
                alloy_primitives::hex::encode(idb)
            ),
        );
        let mut a = node(&da, pa, ka);
        assert!(wait_connected(&a, idb), "first connection");
        a.svc.stop();
        let table =
            std::fs::read_to_string(da.join("peers.json")).unwrap_or_else(|_| String::new());
        assert!(
            table.contains(&alloy_primitives::hex::encode(idb)),
            "{table}"
        );

        // second start: no static, trusted or boot peers, only the table
        let _ = std::fs::remove_file(&static_nodes);
        let mut a2 = node(&da, free_port(), ka);
        assert!(wait_connected(&a2, idb), "reconnect from the peer table");
        a2.svc.stop();
        let mut b = b;
        b.svc.stop();
        let _ = std::fs::remove_dir_all(&root);
    }
}
