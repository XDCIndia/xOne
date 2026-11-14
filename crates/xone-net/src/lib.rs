//! XDC peer-to-peer layer (XB-12): discv4 with the XDC ping, RLPx over `reth-ecies`, the `eth`
//! protocol in its XDC versions 100 (bare framing) and 101 (request-id framing) without ForkID,
//! BFT message gossip, peer scoring and trusted peers. Spec pages: `spec/net/`.
//!
//! Everything that can be pure is pure (codecs, discovery, gossip, admission, scoring); the
//! only tasks are the per-peer sessions in [`peers`]. No global state: every clock reading is
//! passed in or taken from the owning value.
#![forbid(unsafe_code)]

pub mod bft_gossip;
pub mod discv4;
pub mod eth;
pub mod netrestrict;
pub mod peers;
pub mod rlpx;

use rlpx::DisconnectReason;

/// One error type for the transport and session layers. Pure codecs have their own.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("ecies: {0}")]
    Ecies(#[from] reth_ecies::ECIESError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("snappy: {0}")]
    Snappy(#[from] snap::Error),
    #[error("wire: {0}")]
    Wire(#[from] eth::WireError),
    #[error("status: {0}")]
    Status(#[from] eth::status62::StatusError),
    #[error("peer disconnected: {0:?}")]
    Disconnect(DisconnectReason),
    #[error("connection closed")]
    Closed,
    #[error("timeout during {0}")]
    Timeout(&'static str),
    #[error("message code {code:#x}: {len} bytes exceeds the cap of {max}")]
    TooLarge { code: u64, len: usize, max: usize },
    #[error("invalid message code {0:#x}")]
    BadCode(u64),
    #[error("no shared eth version (remote offers {0:?})")]
    NoSharedEth(Vec<rlpx::Cap>),
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    #[error("peer is banned")]
    Banned,
    #[error("node is shutting down")]
    Shutdown,
    #[error("rejected: {0:?}")]
    Rejected(DisconnectReason),
    #[error("{0} is outside --netrestrict")]
    NetRestricted(std::net::IpAddr),
    #[error("unknown peer")]
    UnknownPeer,
    #[error("peer send queue is full")]
    Busy,
}
