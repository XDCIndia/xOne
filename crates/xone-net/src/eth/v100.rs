//! eth/100 (`XDPOS2`): eth/63-shaped data plane, bare requests and responses, no request id
//! (geth-XDC `protocol.go:39`, `handlers.go:41-54`, `peer.go:447-458`, `peer.go:683-685`).

use super::{Framing, Message, WireError};

pub const VERSION: u64 = 100;

pub fn decode(code: u64, body: &[u8]) -> Result<Message, WireError> {
    super::decode(Framing::Bare, code, body)
}

pub fn encode(msg: &Message) -> Result<(u64, Vec<u8>), WireError> {
    super::encode(Framing::Bare, msg)
}
