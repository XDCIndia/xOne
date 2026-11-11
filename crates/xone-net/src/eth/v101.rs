//! eth/101 (`XDPOS2RID`): as eth/100, but headers, bodies and receipts carry a request id, and
//! receipts use the eth/70 shape (`protocol.go:40,52-61`, `handler.go:276-310`). Offered first.

use super::{Framing, Message, WireError};

pub const VERSION: u64 = 101;

pub fn decode(code: u64, body: &[u8]) -> Result<Message, WireError> {
    super::decode(Framing::RequestId, code, body)
}

pub fn encode(msg: &Message) -> Result<(u64, Vec<u8>), WireError> {
    super::encode(Framing::RequestId, msg)
}
