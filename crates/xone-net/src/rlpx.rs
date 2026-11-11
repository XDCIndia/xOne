//! RLPx session over `reth-ecies` (unmodified): the p2p Hello, capability matching, snappy,
//! Ping/Pong, Disconnect, and the message frame codec (`rlp(code) ++ snappy(body)`).
//! geth-XDC: `p2p/peer.go:41-70` (constants, Hello), `p2p/transport.go:39,130-160`
//! (handshake timeout, snappy from p2p version 5), `p2p/rlpx/rlpx.go:146-160` (decompressed
//! size check before allocation), `p2p/peer_error.go:60-75` (disconnect reasons).

use alloy_primitives::B512;
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};
use futures::{SinkExt, StreamExt};
use reth_ecies::stream::ECIESStream;
use secp256k1::{PublicKey, SecretKey};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{eth, NetError};

pub use reth_network_peers::PeerId;

/// Message codes below this belong to the base p2p protocol (`p2p/peer.go:43`). With `eth` the
/// only capability we offer, eth codes start here.
pub const BASE_LEN: u64 = 16;
/// Base protocol version; snappy is on from 5 (`p2p/peer.go:42,46`).
pub const P2P_VERSION: u64 = 5;
/// Cap on Hello, Disconnect, Ping and Pong bodies (ADR-007 section 5: Status and Hello 4 KiB).
pub const BASE_MAX: usize = 4096;

pub const HELLO: u64 = 0x00;
pub const DISCONNECT: u64 = 0x01;
pub const PING: u64 = 0x02;
pub const PONG: u64 = 0x03;

/// The node id of a secret key.
pub fn peer_id(key: &SecretKey) -> PeerId {
    reth_network_peers::pk2id(&PublicKey::from_secret_key_global(key))
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

/// p2p Hello (`p2p/peer.go:60-67`). Fields after `id` are ignored on decode, as geth does, and
/// are never sent.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable)]
pub struct Hello {
    pub version: u64,
    pub name: String,
    pub caps: Vec<Cap>,
    pub port: u64,
    pub id: B512,
}

impl Decodable for Hello {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        if buf.len() < h.payload_length {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let (mut p, rest) = buf.split_at(h.payload_length);
        *buf = rest;
        let p = &mut p;
        Ok(Hello {
            version: Decodable::decode(p)?,
            name: Decodable::decode(p)?,
            caps: Decodable::decode(p)?,
            port: Decodable::decode(p)?,
            id: Decodable::decode(p)?,
        })
    }
}

/// Highest `eth` version offered by both sides (`matchProtocols`, `p2p/peer.go:438-459`).
pub fn shared_eth(local: &[Cap], remote: &[Cap]) -> Option<u64> {
    local
        .iter()
        .filter(|c| c.name == "eth" && remote.contains(c))
        .map(|c| c.version)
        .max()
}

/// Disconnect reasons (`p2p/peer_error.go:60-72`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    Requested,
    NetworkError,
    ProtocolError,
    UselessPeer,
    TooManyPeers,
    AlreadyConnected,
    IncompatibleVersion,
    InvalidIdentity,
    Quitting,
    UnexpectedIdentity,
    SelfConnect,
    ReadTimeout,
    SubprotocolError,
}

impl DisconnectReason {
    pub fn code(self) -> u8 {
        use DisconnectReason::*;
        match self {
            Requested => 0,
            NetworkError => 1,
            ProtocolError => 2,
            UselessPeer => 3,
            TooManyPeers => 4,
            AlreadyConnected => 5,
            IncompatibleVersion => 6,
            InvalidIdentity => 7,
            Quitting => 8,
            UnexpectedIdentity => 9,
            SelfConnect => 10,
            ReadTimeout => 11,
            SubprotocolError => 0x10,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        use DisconnectReason::*;
        Some(match c {
            0 => Requested,
            1 => NetworkError,
            2 => ProtocolError,
            3 => UselessPeer,
            4 => TooManyPeers,
            5 => AlreadyConnected,
            6 => IncompatibleVersion,
            7 => InvalidIdentity,
            8 => Quitting,
            9 => UnexpectedIdentity,
            10 => SelfConnect,
            11 => ReadTimeout,
            0x10 => SubprotocolError,
            _ => return None,
        })
    }

    /// What geth sends: `rlp([]DiscReason{reason})`. `DiscReason` is a byte, so Go encodes the
    /// slice as a one-byte string, which for a reason below 0x80 is the byte itself
    /// (`p2p/peer.go:316`, vector `disconnect_quit`).
    pub fn encode(self) -> Vec<u8> {
        vec![self.code()]
    }

    /// Accepts the one-byte form geth sends, `[reason]` and an integer (other clients), and an
    /// empty body (Requested). An unknown code is Requested, never a panic.
    pub fn decode(body: &[u8]) -> Self {
        let code = match body {
            [c] => Some(*c),
            _ => {
                let mut b = body;
                match alloy_rlp::Header::decode(&mut b) {
                    Ok(h) if h.list => u8::decode(&mut b).ok(),
                    Ok(_) => u8::decode(&mut &body[..]).ok(),
                    Err(_) => None,
                }
            }
        };
        code.and_then(Self::from_code).unwrap_or(Self::Requested)
    }
}

/// `rlp(code) ++ body`, with `body` snappy-compressed when `snappy` is set.
pub fn encode_frame(code: u64, body: &[u8], snappy: bool) -> Result<bytes::Bytes, NetError> {
    let mut out = Vec::with_capacity(body.len() + 12);
    code.encode(&mut out);
    if snappy {
        out.extend_from_slice(&snap::raw::Encoder::new().compress_vec(body)?);
    } else {
        out.extend_from_slice(body);
    }
    Ok(out.into())
}

/// Inverse of [`encode_frame`]. `limit(code)` is the cap on the decompressed body of that code;
/// it is checked against the snappy header before any allocation, so a 16 MiB frame cannot make
/// us allocate more than the cap (`p2p/rlpx/rlpx.go:146-157`).
pub fn decode_frame(
    raw: &[u8],
    snappy: bool,
    limit: impl Fn(u64) -> usize,
) -> Result<(u64, Vec<u8>), NetError> {
    let mut b = raw;
    let code = u64::decode(&mut b)?;
    let max = limit(code);
    let len = if snappy {
        snap::raw::decompress_len(b)?
    } else {
        b.len()
    };
    if len > max {
        return Err(NetError::TooLarge { code, len, max });
    }
    let body = if snappy {
        snap::raw::Decoder::new().decompress_vec(b)?
    } else {
        b.to_vec()
    };
    Ok((code, body))
}

/// What we put in our Hello.
#[derive(Debug, Clone)]
pub struct LocalHello {
    pub name: String,
    pub caps: Vec<Cap>,
}

/// A Hello-negotiated session. `send`/`recv` take eth-relative message codes; the base
/// protocol (Ping, Pong, Disconnect) is handled here.
pub struct Conn<Io> {
    io: ECIESStream<Io>,
    snappy: bool,
    pub remote: PeerId,
    pub hello: Hello,
    /// The shared eth version: 100 or 101.
    pub eth_version: u64,
}

impl<Io: AsyncRead + AsyncWrite + Unpin> Conn<Io> {
    /// Dial side: ECIES handshake to `remote`, then Hello. Bound it with a timeout.
    pub async fn connect(
        io: Io,
        key: SecretKey,
        remote: PeerId,
        local: &LocalHello,
    ) -> Result<Self, NetError> {
        let stream = ECIESStream::connect(io, key, remote).await?;
        Self::hello(stream, &key, local).await
    }

    /// Listen side: ECIES handshake from an unknown dialer, then Hello. Bound it with a timeout.
    pub async fn accept(io: Io, key: SecretKey, local: &LocalHello) -> Result<Self, NetError> {
        let stream = ECIESStream::incoming(io, key).await?;
        Self::hello(stream, &key, local).await
    }

    async fn hello(
        mut io: ECIESStream<Io>,
        key: &SecretKey,
        local: &LocalHello,
    ) -> Result<Self, NetError> {
        let remote = io.remote_id();
        let ours = Hello {
            version: P2P_VERSION,
            name: local.name.clone(),
            caps: local.caps.clone(),
            port: 0,
            id: peer_id(key),
        };
        io.send(encode_frame(HELLO, &alloy_rlp::encode(&ours), false)?)
            .await?;
        let raw = io.next().await.ok_or(NetError::Closed)??;
        let (code, body) = decode_frame(&raw, false, |_| BASE_MAX)?;
        match code {
            HELLO => {}
            DISCONNECT => return Err(NetError::Disconnect(DisconnectReason::decode(&body))),
            _ => return Err(NetError::Protocol("first message is not Hello")),
        }
        let hello = Hello::decode(&mut &body[..])?;
        if hello.id != remote {
            return Err(NetError::Rejected(DisconnectReason::UnexpectedIdentity));
        }
        let eth_version = shared_eth(&local.caps, &hello.caps)
            .ok_or_else(|| NetError::NoSharedEth(hello.caps.clone()))?;
        Ok(Self {
            io,
            snappy: hello.version >= P2P_VERSION,
            remote,
            hello,
            eth_version,
        })
    }

    async fn send_base(&mut self, code: u64, body: &[u8]) -> Result<(), NetError> {
        let frame = encode_frame(code, body, self.snappy)?;
        self.io.send(frame).await?;
        Ok(())
    }

    /// Send an eth message; `code` is eth-relative.
    pub async fn send(&mut self, code: u64, body: &[u8]) -> Result<(), NetError> {
        self.send_base(BASE_LEN + code, body).await
    }

    /// Next eth message as `(eth-relative code, body)`. Ping is answered, Pong is skipped,
    /// Disconnect is an error. Codes 4 to 15 are reserved and a protocol error.
    pub async fn recv(&mut self) -> Result<(u64, Vec<u8>), NetError> {
        loop {
            let raw = self.io.next().await.ok_or(NetError::Closed)??;
            let (code, body) =
                decode_frame(&raw, self.snappy, |c| match c.checked_sub(BASE_LEN) {
                    Some(eth_code) => eth::codes::max_size(eth_code),
                    None => BASE_MAX,
                })?;
            match code {
                DISCONNECT => return Err(NetError::Disconnect(DisconnectReason::decode(&body))),
                PING => self.send_base(PONG, &[alloy_rlp::EMPTY_LIST_CODE]).await?,
                PONG => {}
                HELLO => return Err(NetError::Protocol("second Hello")),
                c if c >= BASE_LEN => return Ok((c - BASE_LEN, body)),
                c => return Err(NetError::BadCode(c)),
            }
        }
    }

    /// Best effort: the caller bounds it with a timeout.
    pub async fn disconnect(&mut self, reason: DisconnectReason) -> Result<(), NetError> {
        self.send_base(DISCONNECT, &reason.encode()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_reason_round_trips_and_tolerates_shapes() {
        for c in (0..=11).chain([0x10]) {
            let r = DisconnectReason::from_code(c).unwrap();
            assert_eq!(r.code(), c);
            assert_eq!(DisconnectReason::decode(&r.encode()), r);
        }
        assert_eq!(DisconnectReason::from_code(12), None);
        // bare integer, empty body and garbage never panic
        assert_eq!(
            DisconnectReason::decode(&[0xc1, 0x08]),
            DisconnectReason::Quitting
        );
        assert_eq!(
            DisconnectReason::decode(&[0xc1, 0x80]),
            DisconnectReason::Requested
        );
        assert_eq!(
            DisconnectReason::decode(&[0x80]),
            DisconnectReason::Requested
        );
        assert_eq!(DisconnectReason::decode(&[]), DisconnectReason::Requested);
        assert_eq!(
            DisconnectReason::decode(&[0xff; 9]),
            DisconnectReason::Requested
        );
        assert_eq!(DisconnectReason::Requested.encode(), [0x00]);
    }

    #[test]
    fn frame_round_trip_and_cap_before_allocation() {
        for snappy in [false, true] {
            let f = encode_frame(0x13, b"hello", snappy).unwrap();
            assert_eq!(
                decode_frame(&f, snappy, |_| 5).unwrap(),
                (0x13, b"hello".to_vec())
            );
            assert!(matches!(
                decode_frame(&f, snappy, |_| 4),
                Err(NetError::TooLarge { len: 5, max: 4, .. })
            ));
        }
        // A snappy header that claims 1 GiB is refused without decompressing.
        let mut bomb = vec![0x01];
        bomb.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x04, 0x00]);
        assert!(matches!(
            decode_frame(&bomb, true, |_| 1024),
            Err(NetError::TooLarge { .. })
        ));
        assert!(decode_frame(&[0x81, 0x00], false, |_| 1).is_err()); // non-canonical code
    }

    #[test]
    fn hello_ignores_trailing_fields_and_matching_picks_highest() {
        let h = Hello {
            version: 5,
            name: "x".into(),
            caps: vec![Cap::eth(101), Cap::eth(100)],
            port: 0,
            id: B512::repeat_byte(7),
        };
        let mut payload = alloy_rlp::encode(&h);
        // append one extra list element and fix the list header (`0xf8 len`: the payload is over 55 bytes)
        payload.push(0x05);
        assert_eq!(payload[0], 0xf8);
        payload[1] += 1;
        assert_eq!(Hello::decode(&mut &payload[..]).unwrap(), h);
        assert_eq!(
            shared_eth(&h.caps, &[Cap::eth(100), Cap::eth(63)]),
            Some(100)
        );
        assert_eq!(
            shared_eth(&h.caps, &[Cap::eth(101), Cap::eth(100)]),
            Some(101)
        );
        assert_eq!(shared_eth(&h.caps, &[Cap::eth(63)]), None);
    }
}
