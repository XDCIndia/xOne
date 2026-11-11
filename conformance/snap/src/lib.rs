//! Test-only glue between `xone-snap` and `xone-net`: snap/1 over an RLPx session.
//!
//! snap/1 shares the connection with eth. Codes are assigned after the eth messages, in
//! capability order (`p2p/peer.go:438-459`): with eth/100 or eth/101 (229 messages,
//! `eth/protocols/eth/protocol.go:89`) snap code `c` is eth-relative code `229 + c`, which
//! `xone_net::rlpx::Conn` puts on the wire as `16 + 229 + c`. While a snap request is
//! outstanding the eth messages that arrive (gossip, votes) are read and dropped, so that they
//! cannot block the answer (xOneGo `a3a92e02`).

use alloy_primitives::{B256, B512, U256};
use secp256k1::SecretKey;
use std::time::Duration;
use tokio::{net::TcpStream, runtime::Handle, time::timeout};
use xone_net::{
    eth::{self, codes::MESSAGE_COUNT, status62::Local, Framing, GetHeaders, Origin, Status62},
    rlpx::{Cap, Conn, LocalHello, PeerId},
    NetError,
};
use xone_primitives::Header;
use xone_snap::{
    client::{PeerError, PeerSource},
    codec::Message,
    server::{Handled, Server},
};

/// Eth-relative code of snap message 0.
pub const SNAP_BASE: u64 = MESSAGE_COUNT;

pub fn hello() -> LocalHello {
    LocalHello {
        name: "xone-snap-test".into(),
        caps: vec![
            Cap::eth(101),
            Cap::eth(100),
            Cap {
                name: "snap".into(),
                version: 1,
            },
        ],
    }
}

pub fn has_snap(c: &Conn<TcpStream>) -> bool {
    c.hello
        .caps
        .iter()
        .any(|x| x.name == "snap" && x.version == 1)
}

fn framing(version: u64) -> Framing {
    if version == 101 {
        Framing::RequestId
    } else {
        Framing::Bare
    }
}

/// Dials, runs Hello and the eth Status exchange (geth registers the snap extension only for an
/// eth peer that passed Status) and returns the session with the remote Status.
pub async fn dial(
    addr: std::net::SocketAddr,
    remote: PeerId,
    key: SecretKey,
    network_id: u64,
    genesis: B256,
) -> Result<(Conn<TcpStream>, Status62), NetError> {
    let tcp = timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| NetError::Timeout("dial"))??;
    let mut conn = timeout(
        Duration::from_secs(5),
        Conn::connect(tcp, key, remote, &hello()),
    )
    .await
    .map_err(|_| NetError::Timeout("handshake"))??;
    let f = framing(conn.eth_version);
    let ours = Status62 {
        version: u32::try_from(conn.eth_version).unwrap_or(0),
        network_id,
        td: U256::from(1),
        head: genesis,
        genesis,
    };
    let (code, body) = eth::encode(f, &eth::Message::Status(ours))?;
    conn.send(code, &body).await?;
    let (code, body) = timeout(Duration::from_secs(5), conn.recv())
        .await
        .map_err(|_| NetError::Timeout("status"))??;
    let eth::Message::Status(theirs) = eth::decode(f, code, &body)? else {
        return Err(NetError::Protocol("first eth message is not Status"));
    };
    theirs.validate(
        &Local {
            network_id,
            genesis,
        },
        conn.eth_version,
    )?;
    Ok((conn, theirs))
}

/// The header with `hash`, over eth. Other messages that arrive first are dropped.
pub async fn header_by_hash(conn: &mut Conn<TcpStream>, hash: B256) -> Result<Header, NetError> {
    let f = framing(conn.eth_version);
    let id = (f == Framing::RequestId).then_some(1);
    let req = eth::Message::GetBlockHeaders {
        id,
        query: GetHeaders {
            origin: Origin::Hash(hash),
            amount: 1,
            skip: 0,
            reverse: false,
        },
    };
    let (code, body) = eth::encode(f, &req)?;
    conn.send(code, &body).await?;
    loop {
        let (code, body) = timeout(Duration::from_secs(10), conn.recv())
            .await
            .map_err(|_| NetError::Timeout("headers"))??;
        if code >= SNAP_BASE {
            continue;
        }
        if let Ok(eth::Message::BlockHeaders { headers, .. }) = eth::decode(f, code, &body) {
            return headers
                .into_iter()
                .next()
                .ok_or(NetError::Protocol("no header"));
        }
    }
}

/// A snap/1 peer over RLPx for the synchronous client driver. Run the driver with
/// `tokio::task::spawn_blocking`: `exchange` blocks on the runtime handle.
pub struct SnapPeer {
    pub handle: Handle,
    pub conn: Conn<TcpStream>,
    pub reply_timeout: Duration,
    /// eth frames read and dropped while waiting for snap answers.
    pub eth_dropped: usize,
    pub exchanges: usize,
}

impl SnapPeer {
    pub fn new(handle: Handle, conn: Conn<TcpStream>) -> Self {
        Self {
            handle,
            conn,
            reply_timeout: Duration::from_secs(20),
            eth_dropped: 0,
            exchanges: 0,
        }
    }
}

impl PeerSource for SnapPeer {
    fn exchange(&mut self, req: Message) -> Result<Message, PeerError> {
        let Self {
            handle,
            conn,
            reply_timeout,
            eth_dropped,
            exchanges,
        } = self;
        *exchanges += 1;
        handle.block_on(async {
            conn.send(SNAP_BASE + req.code(), &req.encode())
                .await
                .map_err(|_| PeerError::Disconnected)?;
            loop {
                let (code, body) = timeout(*reply_timeout, conn.recv())
                    .await
                    .map_err(|_| PeerError::Timeout)?
                    .map_err(|_| PeerError::Disconnected)?;
                match code.checked_sub(SNAP_BASE) {
                    None => *eth_dropped += 1,
                    Some(c) => {
                        return Message::decode(c, &body).map_err(|_| PeerError::Disconnected)
                    }
                }
            }
        })
    }
}

/// Serves snap/1 requests on `conn` until the peer goes away. Returns the number of requests
/// answered.
pub async fn serve(
    conn: &mut Conn<TcpStream>,
    server: &mut Server,
    start: std::time::Instant,
) -> usize {
    let peer: B512 = conn.remote;
    let mut served = 0;
    while let Ok((code, body)) = conn.recv().await {
        let Some(c) = code.checked_sub(SNAP_BASE) else {
            continue; // eth traffic is not ours
        };
        let Ok(msg) = Message::decode(c, &body) else {
            break;
        };
        let now = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let reply = match server.handle(peer, now, &msg) {
            Handled::Reply(m) | Handled::RateExceeded { reply: m, .. } => m,
            Handled::Disconnect => break,
        };
        if conn
            .send(SNAP_BASE + reply.code(), &reply.encode())
            .await
            .is_err()
        {
            break;
        }
        served += 1;
    }
    served
}

pub fn key() -> SecretKey {
    SecretKey::from_slice(B256::random().as_slice()).expect("32 random bytes are a key")
}
