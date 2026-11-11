//! BFT messages through the gossip filter, and discv4 packets. Never panics; an accepted BFT
//! message re-encodes to the input; each accepted message is delivered once.
#![no_main]
use libfuzzer_sys::fuzz_target;
use alloy_primitives::B512;
use xone_net::bft_gossip::{Gossip, Receive};
use xone_net::eth::{codes, BftMessage};

fuzz_target!(|data: &[u8]| {
    let Some((sel, body)) = data.split_first() else { return };
    let code = codes::VOTE + u64::from(sel % 3);
    if let Ok(m) = BftMessage::decode_body(code, body) {
        assert_eq!(m.encode_body(), body);
    }
    let peer = B512::repeat_byte(1);
    let mut g = Gossip::default();
    g.add_peer(peer);
    let first = g.receive(&peer, code, body);
    if matches!(first, Receive::Accepted { .. }) {
        assert_eq!(g.receive(&peer, code, body), Receive::Duplicate);
    }
    let _ = xone_net::discv4::decode(data);
});
