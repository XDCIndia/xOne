//! Status, Hello, Disconnect and the RLPx frame decoder. Never panics; an accepted Status
//! validates without panicking and re-encodes to five fields.
#![no_main]
use alloy_primitives::B256;
use alloy_rlp::Decodable;
use libfuzzer_sys::fuzz_target;
use xone_net::eth::status62::{Local, Status62};
use xone_net::eth::{self, codes, Framing};
use xone_net::rlpx::{decode_frame, DisconnectReason, Hello};

fuzz_target!(|data: &[u8]| {
    let local = Local { network_id: 5551, genesis: B256::ZERO };
    if let Ok(s) = Status62::decode(&mut &data[..]) {
        let _ = s.validate(&local, 101);
        let _ = s.rank();
        assert_eq!(Status62::decode(&mut &alloy_rlp::encode(&s)[..]).unwrap(), s);
    }
    for f in [Framing::Bare, Framing::RequestId] {
        let _ = eth::decode(f, codes::STATUS, data);
    }
    let _ = Hello::decode(&mut &data[..]);
    let _ = DisconnectReason::decode(data);
    for snappy in [false, true] {
        let _ = decode_frame(data, snappy, codes::max_size);
    }
});
