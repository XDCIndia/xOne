//! Header, body, receipt and the other data-plane decoders of eth/100 and eth/101. The first
//! byte picks the framing and the message code. Whatever decodes must encode back to the same
//! bytes (canonical RLP only).
#![no_main]
use libfuzzer_sys::fuzz_target;
use xone_net::eth::{self, codes, Framing};

const CODES: [u64; 15] = [
    codes::NEW_BLOCK_HASHES,
    codes::TRANSACTIONS,
    codes::GET_BLOCK_HEADERS,
    codes::BLOCK_HEADERS,
    codes::GET_BLOCK_BODIES,
    codes::BLOCK_BODIES,
    codes::NEW_BLOCK,
    codes::NEW_POOLED_TX_HASHES,
    codes::GET_POOLED_TXS,
    codes::POOLED_TXS,
    codes::GET_NODE_DATA,
    codes::NODE_DATA,
    codes::GET_RECEIPTS,
    codes::RECEIPTS,
    codes::BLOCK_RANGE_UPDATE,
];

fuzz_target!(|data: &[u8]| {
    let Some((sel, body)) = data.split_first() else { return };
    let f = if sel & 1 == 0 { Framing::Bare } else { Framing::RequestId };
    let code = CODES[usize::from(sel >> 1) % CODES.len()];
    if let Ok(msg) = eth::decode(f, code, body) {
        if let Ok((c, again)) = eth::encode(f, &msg) {
            assert_eq!((c, again.as_slice()), (code, body), "non-canonical input accepted");
        }
    }
});
