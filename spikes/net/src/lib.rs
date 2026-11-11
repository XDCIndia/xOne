//! XA-03 spike S2: the smallest devp2p session that can speak the XDC eth/100 and eth/101 wire
//! over `reth-ecies`. reth's own `P2PStream` cannot (REPORT.md, "What needed a workaround"), so
//! the p2p layer (Hello, snappy, Ping/Pong, message-id offset) is rebuilt here in about 150 lines.
//! Test-only: nothing here is shipped.

use std::net::SocketAddr;

use alloy_primitives::{B256, B512, U256};
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use futures::{SinkExt, StreamExt};
use reth_ecies::stream::ECIESStream;
use reth_network_peers::PeerId;
use secp256k1::SecretKey;
use tokio::net::TcpStream;

/// XDC `XDPOS2`: eth/63-shaped data plane, bare requests (geth-XDC `eth/protocols/eth/protocol.go:39`).
pub const ETH_100: u64 = 100;
/// XDC `XDPOS2RID`: as eth/100 but headers, bodies and receipts carry a request id (`protocol.go:40`).
pub const ETH_101: u64 = 101;
/// Message ids below this belong to the base p2p protocol (geth `p2p/peer.go:43`).
pub const BASE_LEN: u64 = 16;
/// Eth message codes (geth `eth/protocols/eth/protocol.go:98-127`).
pub const STATUS: u64 = 0x00;
pub const GET_BLOCK_HEADERS: u64 = 0x03;
pub const BLOCK_HEADERS: u64 = 0x04;
pub const VOTE: u64 = 0xe0;
pub const TIMEOUT: u64 = 0xe1;
pub const SYNC_INFO: u64 = 0xe2;
/// Messages of eth/100 and eth/101 (geth `protocol.go:89`, `XDPOS2: 229, XDPOS2RID: 229`).
pub const XDPOS2_LEN: u64 = 229;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ecies: {0}")]
    Ecies(#[from] reth_ecies::ECIESError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("snappy: {0}")]
    Snappy(#[from] snap::Error),
    #[error("peer disconnected, reason {0:?}")]
    Disconnect(Option<u64>),
    #[error("connection closed")]
    Closed,
    #[error("no shared eth version (peer offered {0:?})")]
    NoSharedEth(Vec<Cap>),
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Cap {
    pub name: String,
    pub version: u64,
}

impl Cap {
    pub fn eth(version: u64) -> Self {
        Self {
            name: "eth".into(),
            version,
        }
    }
}

/// p2p Hello (geth `p2p/peer.go:60-67`; geth ignores extra trailing fields, this type does not send any).
#[derive(Debug, Clone, RlpEncodable, RlpDecodable)]
pub struct Hello {
    pub version: u64,
    pub name: String,
    pub caps: Vec<Cap>,
    pub port: u64,
    pub id: B512,
}

/// XDC status for eth/100 and eth/101: five fields, no fork id (`protocol.go:196-202`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Status62 {
    pub version: u32,
    pub network_id: u64,
    pub td: U256,
    pub head: B256,
    pub genesis: B256,
}

/// `GetBlockHeadersRequest` with a block-number origin (`protocol.go:226-231`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GetHeaders {
    pub origin: u64,
    pub amount: u64,
    pub skip: u64,
    pub reverse: bool,
}

/// eth/101 request: id, then the same body (`protocol.go:234-237`).
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct GetHeadersRid {
    pub request_id: u64,
    pub request: GetHeaders,
}

/// Highest version of `eth` offered by both sides (geth `matchProtocols`, `p2p/peer.go:438-459`).
pub fn shared_eth(local: &[Cap], remote: &[Cap]) -> Option<u64> {
    local
        .iter()
        .filter(|c| c.name == "eth" && remote.contains(c))
        .map(|c| c.version)
        .max()
}

/// A connected, Hello-negotiated session. Message codes passed to `send`/`recv` are absolute
/// (base protocol included); `send_eth`/`recv_eth` add and remove the eth offset.
pub struct Session {
    io: ECIESStream<TcpStream>,
    snappy: bool,
    pub peer: Hello,
    pub eth_version: u64,
}

impl Session {
    /// Dial, run the ECIES handshake, exchange Hello, and pick the shared `eth` version.
    pub async fn connect(
        addr: SocketAddr,
        remote: PeerId,
        key: SecretKey,
        caps: Vec<Cap>,
    ) -> Result<Self, Error> {
        let tcp = TcpStream::connect(addr).await?;
        let mut io = ECIESStream::connect(tcp, key, remote).await?;
        let id = reth_network_peers::pk2id(&secp256k1::PublicKey::from_secret_key_global(&key));
        let hello = Hello {
            version: 5,
            name: "net-spike".into(),
            caps: caps.clone(),
            port: 0,
            id,
        };
        io.send(frame(0, &alloy_rlp::encode(&hello))).await?;
        let (code, body) = Self::read(&mut io, false).await?;
        if code == 1 {
            return Err(Error::Disconnect(disconnect_reason(&body)));
        }
        let peer = Hello::decode(&mut &body[..])?;
        let eth_version =
            shared_eth(&caps, &peer.caps).ok_or(Error::NoSharedEth(peer.caps.clone()))?;
        // p2p version 5 or higher turns snappy on for every message after Hello (geth `p2p/transport.go:149`).
        Ok(Self {
            io,
            snappy: peer.version >= 5,
            peer,
            eth_version,
        })
    }

    async fn read(io: &mut ECIESStream<TcpStream>, snappy: bool) -> Result<(u64, Vec<u8>), Error> {
        let raw = io.next().await.ok_or(Error::Closed)??;
        let mut b = &raw[..];
        let code = u64::decode(&mut b)?;
        let body = if snappy {
            snap::raw::Decoder::new().decompress_vec(b)?
        } else {
            b.to_vec()
        };
        Ok((code, body))
    }

    pub async fn send(&mut self, code: u64, payload: &[u8]) -> Result<(), Error> {
        let body = if self.snappy {
            snap::raw::Encoder::new().compress_vec(payload)?
        } else {
            payload.to_vec()
        };
        self.io.send(frame(code, &body)).await?;
        Ok(())
    }

    /// Next non-base-protocol message. Ping is answered, Disconnect is an error.
    pub async fn recv(&mut self) -> Result<(u64, Vec<u8>), Error> {
        loop {
            let (code, body) = Self::read(&mut self.io, self.snappy).await?;
            match code {
                1 => return Err(Error::Disconnect(disconnect_reason(&body))),
                2 => self.send(3, &[alloy_rlp::EMPTY_LIST_CODE]).await?,
                3 => {}
                _ => return Ok((code, body)),
            }
        }
    }

    pub async fn send_eth(&mut self, code: u64, payload: &[u8]) -> Result<(), Error> {
        self.send(BASE_LEN + code, payload).await
    }

    pub async fn recv_eth(&mut self) -> Result<(u64, Vec<u8>), Error> {
        let (code, body) = self.recv().await?;
        Ok((code.saturating_sub(BASE_LEN), body))
    }

    /// Wait for the eth message with `code`, skipping gossip such as NewBlockHashes.
    pub async fn expect_eth(&mut self, code: u64) -> Result<Vec<u8>, Error> {
        loop {
            let (c, body) = self.recv_eth().await?;
            if c == code {
                return Ok(body);
            }
        }
    }

    pub async fn disconnect(&mut self) -> Result<(), Error> {
        // reason 0x08: client quitting
        self.send(1, &alloy_rlp::encode([8u8])).await
    }
}

/// `rlp(code) ++ body`, the unit the RLPx frame carries.
fn frame(code: u64, body: &[u8]) -> bytes::Bytes {
    let mut out = Vec::with_capacity(body.len() + 9);
    code.encode(&mut out);
    out.extend_from_slice(body);
    out.into()
}

fn disconnect_reason(body: &[u8]) -> Option<u64> {
    // `[reason]` (or a bare integer from some clients)
    let mut b = body;
    let h = alloy_rlp::Header::decode(&mut b).ok()?;
    if h.list {
        u64::decode(&mut b).ok()
    } else {
        u64::decode(&mut &body[..]).ok()
    }
}
