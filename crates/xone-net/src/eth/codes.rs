//! Message codes of eth/100 and eth/101 (geth-XDC `eth/protocols/eth/protocol.go:97-128`) and the
//! size caps of ADR-007 section 5. Codes are eth-relative (the RLPx offset 16 is added by
//! [`crate::rlpx::Conn`]).

pub const STATUS: u64 = 0x00;
pub const NEW_BLOCK_HASHES: u64 = 0x01;
pub const TRANSACTIONS: u64 = 0x02;
pub const GET_BLOCK_HEADERS: u64 = 0x03;
pub const BLOCK_HEADERS: u64 = 0x04;
pub const GET_BLOCK_BODIES: u64 = 0x05;
pub const BLOCK_BODIES: u64 = 0x06;
pub const NEW_BLOCK: u64 = 0x07;
/// `OrderTxMsg` on legacy XDC peers; drained without decoding (`handler.go:238-246`).
pub const NEW_POOLED_TX_HASHES: u64 = 0x08;
/// `LendingTxMsg` on legacy XDC peers; drained without decoding (`handler.go:265-268`).
pub const GET_POOLED_TXS: u64 = 0x09;
pub const POOLED_TXS: u64 = 0x0a;
pub const GET_NODE_DATA: u64 = 0x0d;
pub const NODE_DATA: u64 = 0x0e;
pub const GET_RECEIPTS: u64 = 0x0f;
pub const RECEIPTS: u64 = 0x10;
pub const BLOCK_RANGE_UPDATE: u64 = 0x11;
pub const VOTE: u64 = 0xe0;
pub const TIMEOUT: u64 = 0xe1;
pub const SYNC_INFO: u64 = 0xe2;

/// Number of message codes of versions 100 and 101 (`protocol.go:89`); a code at or above this is
/// invalid.
pub const MESSAGE_COUNT: u64 = 229;
/// geth `maxMessageSize` (`protocol.go:92`): every other message.
pub const MAX_MESSAGE: usize = 10 * 1024 * 1024;
/// ADR-007 section 5. A round has at most 108 signers of 65 bytes in each certificate.
pub const MAX_VOTE: usize = 1024;
pub const MAX_TIMEOUT: usize = 1024;
pub const MAX_SYNC_INFO: usize = 32 * 1024;
pub const MAX_STATUS: usize = 4 * 1024;

/// Cap on the decompressed body of `code`, checked before the body is read.
pub fn max_size(code: u64) -> usize {
    match code {
        VOTE => MAX_VOTE,
        TIMEOUT => MAX_TIMEOUT,
        SYNC_INFO => MAX_SYNC_INFO,
        STATUS => MAX_STATUS,
        _ => MAX_MESSAGE,
    }
}

pub fn is_bft(code: u64) -> bool {
    matches!(code, VOTE | TIMEOUT | SYNC_INFO)
}
