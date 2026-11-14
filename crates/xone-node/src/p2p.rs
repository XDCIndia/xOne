//! devp2p tooling (issue #178, first slice): node keys, enode URLs and a discovery-only bootnode.
//!
//! `xone p2p key generate` writes a node key in the format of `<datadir>/nodekey` (64 hex digits,
//! no newline, as geth `crypto.SaveECDSA`); `xone p2p key enode` prints the enode URL of a key;
//! `xone p2p bootnode` answers discv4 (ping, pong, findnode, neighbors) with no chain and no RLPx,
//! like geth's `cmd/bootnode`.

use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use secp256k1::SecretKey;
use xone_net::{discv4::Discv4, rlpx::peer_id};

/// A fresh random node key.
pub fn generate_key() -> Result<SecretKey, String> {
    loop {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| e.to_string())?;
        if let Ok(k) = SecretKey::from_slice(&b) {
            return Ok(k);
        }
    }
}

/// Writes `key` to a new file `path` (refuses to overwrite), owner-only on Unix.
pub fn write_key(path: &Path, key: &SecretKey) -> Result<(), String> {
    use std::io::Write as _;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(alloy_primitives::hex::encode(key.secret_bytes()).as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads a node key file (hex, surrounding whitespace allowed).
pub fn read_key(path: &Path) -> Result<SecretKey, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b =
        alloy_primitives::hex::decode(t.trim()).map_err(|e| format!("{}: {e}", path.display()))?;
    SecretKey::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))
}

/// `enode://<id>@<ip>:<tcp>` with `?discport=` when the UDP port differs (geth `enode.Node.URLv4`).
pub fn enode_url(key: &SecretKey, ip: IpAddr, tcp: u16, udp: u16) -> String {
    let id = alloy_primitives::hex::encode(peer_id(key));
    let host = match ip {
        IpAddr::V6(a) => format!("[{a}]"),
        IpAddr::V4(a) => a.to_string(),
    };
    if tcp == udp {
        format!("enode://{id}@{host}:{tcp}")
    } else {
        format!("enode://{id}@{host}:{tcp}?discport={udp}")
    }
}

fn now_ms() -> u64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// Runs a discovery-only node on `sock` until `stop` is set: answers and bonds, keeps the table
/// fresh, and serves findnode from it. No chain, no RLPx listener.
pub fn bootnode(
    key: SecretKey,
    sock: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let local = sock.local_addr().map_err(|e| e.to_string())?;
    sock.set_nonblocking(true).map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let mut sock = tokio::net::UdpSocket::from_std(sock).map_err(|e| e.to_string())?;
        let mut d = Discv4::new(key, local, local.port());
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        let mut buf = vec![0u8; 2048];
        while !stop.load(SeqCst) {
            tokio::select! {
                r = sock.recv_from(&mut buf) => {
                    if let Ok((n, from)) = r {
                        // an invalid or unsolicited packet is dropped, as the protocol says
                        let _ = d.handle(now_ms(), from, &buf[..n], &mut sock);
                    }
                }
                _ = tick.tick() => d.expire(now_ms()),
            }
        }
        Ok(())
    })
}

/// A running bootnode: its bound address, the stop flag and its thread.
pub type Running = (
    SocketAddr,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<(), String>>,
);

/// The bootnode on its own thread, for tests and embedding; `stop` ends it.
pub fn spawn_bootnode(key: SecretKey, addr: SocketAddr) -> Result<Running, String> {
    let sock = std::net::UdpSocket::bind(addr).map_err(|e| format!("bind {addr}: {e}"))?;
    let bound = sock.local_addr().map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let th = std::thread::Builder::new()
        .name("bootnode".into())
        .spawn(move || bootnode(key, sock, s))
        .map_err(|e| e.to_string())?;
    Ok((bound, stop, th))
}
