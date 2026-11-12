//! Test doubles: an in-memory peer that serves from a [`Snapshot`] through the real codec and
//! the real [`Server`] admission, and a deterministic state with wide balances, storage and code.

use alloy_primitives::{keccak256, Address, Bytes, B256, B512, U256};
use std::collections::BTreeMap;
use xone_primitives::WideBalance;
use xone_state::{Account, FlatWrite, MemFlat};

use crate::{
    client::{PeerError, PeerSource},
    codec::Message,
    server::{Handled, Server},
};

/// A peer that is a [`Server`] in this process. Every request and every answer goes through
/// `encode` and `decode`, so the codec is in the loop. `log` keeps the requests; `budget` is the
/// number of exchanges before the peer disconnects (a kill).
pub struct Loopback {
    pub server: Server,
    pub peer: B512,
    pub now_ms: u64,
    /// Milliseconds that pass at every exchange (the Q15 bucket refills with them).
    pub tick_ms: u64,
    pub log: Vec<Message>,
    pub budget: Option<usize>,
}

impl Loopback {
    pub fn new(server: Server) -> Self {
        Self {
            server,
            peer: B512::repeat_byte(1),
            now_ms: 0,
            tick_ms: 100,
            log: Vec::new(),
            budget: None,
        }
    }
}

impl PeerSource for Loopback {
    fn exchange(&mut self, req: Message) -> Result<Message, PeerError> {
        if let Some(b) = &mut self.budget {
            if *b == 0 {
                return Err(PeerError::Disconnected);
            }
            *b -= 1;
        }
        let wire =
            Message::decode(req.code(), &req.encode()).map_err(|_| PeerError::Disconnected)?;
        self.log.push(req);
        self.now_ms += self.tick_ms;
        let reply = match self.server.handle(self.peer, self.now_ms, &wire) {
            Handled::Reply(m) | Handled::RateExceeded { reply: m, .. } => m,
            Handled::Disconnect => return Err(PeerError::Disconnected),
        };
        Message::decode(reply.code(), &reply.encode()).map_err(|_| PeerError::Disconnected)
    }
}

/// The address of account `i` of [`flat_state`].
pub fn addr(i: usize) -> Address {
    Address::from_slice(&h("addr", i)[12..])
}

fn h(tag: &str, i: usize) -> B256 {
    keccak256(format!("{tag}-{i}"))
}

/// `n` accounts as address-keyed flat tables, and their code. Every 97th has a balance above
/// 2^256 (2^256 + i; 2^264 - 1 for every 1,000th), every 10th has storage (account 0 has 3,000
/// slots, the others 1 to 20), every 7th has code.
pub fn flat_state(n: usize) -> (MemFlat, BTreeMap<B256, Bytes>) {
    let (mut flat, mut codes) = (MemFlat::recording(), BTreeMap::new());
    for i in 0..n {
        let balance = if i > 0 && i % 1000 == 0 {
            WideBalance::from_be_bytes(&[&[0u8][..], &[0xff; 33][..]].concat()) // 2^264 - 1
        } else if i % 97 == 0 {
            let two_256 = WideBalance::from_be_bytes(&[&[1u8][..], &[0u8; 32][..]].concat());
            two_256.checked_add(U256::from(i)).unwrap_or(two_256)
        } else {
            WideBalance::from(U256::from(i) * U256::from(10u64).pow(U256::from(15u8)))
        };
        let mut acc = Account {
            nonce: u64::try_from(i).unwrap_or(0),
            balance,
            ..Account::default()
        };
        let addr = addr(i);
        if i % 7 == 0 {
            let code: Vec<u8> = (0..20 + i % 180)
                .map(|j| u8::try_from((i + j) % 251).unwrap_or(0))
                .collect();
            acc.code_hash = keccak256(&code);
            codes.insert(acc.code_hash, Bytes::from(code));
        }
        flat.set_account(addr, Some(acc));
        if i % 10 == 0 {
            let count = if i == 0 { 3000 } else { 1 + i % 20 };
            for s in 0..count {
                flat.set_slot(
                    addr,
                    h(&format!("slot{i}"), s),
                    U256::from(s + 1) << (8 * (s % 30)),
                );
            }
        }
    }
    (flat, codes)
}
