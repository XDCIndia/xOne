//! Gossip of BFT messages (Vote 0xe0, Timeout 0xe1, SyncInfo 0xe2): size caps, strict decode,
//! deduplication and one-time delivery to each peer. Pure state, no I/O.
//!
//! geth-XDC: `eth/bft/handler.go:32-56` (known-message caches keyed by the message hash, 131072
//! each; bounded forward queue), `:185-229` (dedup before verification), `eth/handler_xdc.go:28-100`
//! (broadcast to all peers), `eth/protocols/eth/handlers.go:1157-1185` (bodies are bare RLP).
//! geth drops a malformed message silently; ADR-007 section 5 scores it (Malformed, 10 points).
//!
//! Flow: [`Gossip::receive`] checks and dedups an inbound message. After the consensus engine
//! accepts it, [`Gossip::targets`] says which peers still need it. A message we create goes
//! through [`Gossip::originate`].

use std::collections::{BTreeMap, HashSet, VecDeque};

use alloy_primitives::{keccak256, B256};
use xone_node::Offence;

use crate::eth::codes;
pub use crate::eth::BftMessage;
use crate::rlpx::PeerId;

/// Hashes remembered per peer. A round has at most 324 messages (108 masternodes, three kinds),
/// so this holds about three rounds. A peer costs about 100 KiB.
pub const PER_PEER_KNOWN: usize = 1024;
/// Hashes remembered across peers. geth keeps 131072 per kind (about 4 MB each); this is a
/// proposal sized to a few hundred rounds. ponytail: tune with the 6-node devnet.
pub const GLOBAL_KNOWN: usize = 32 * 1024;

/// A set of at most `cap` hashes. The oldest is forgotten first, so a message older than `cap`
/// newer ones can be delivered again; consumers drop stale rounds anyway.
#[derive(Debug)]
pub struct Known {
    set: HashSet<B256>,
    order: VecDeque<B256>,
    cap: usize,
}

impl Known {
    pub fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub fn contains(&self, h: &B256) -> bool {
        self.set.contains(h)
    }

    /// `true` if `h` was new.
    pub fn insert(&mut self, h: B256) -> bool {
        if !self.set.insert(h) {
            return false;
        }
        self.order.push_back(h);
        if self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// Outcome of an inbound message.
#[derive(Debug, PartialEq, Eq)]
pub enum Receive {
    /// First time we see it: verify it, then call [`Gossip::targets`].
    Accepted { msg: BftMessage, hash: B256 },
    /// Already seen (from any peer). Not an offence: honest peers relay the same message.
    Duplicate,
    /// The sender is not registered; the message is dropped.
    UnknownPeer,
    /// Score the sender with this offence; the message is dropped.
    Reject(Offence),
}

#[derive(Debug)]
pub struct Gossip {
    global: Known,
    peers: BTreeMap<PeerId, Known>,
    per_peer: usize,
}

impl Default for Gossip {
    fn default() -> Self {
        Self::new(GLOBAL_KNOWN, PER_PEER_KNOWN)
    }
}

impl Gossip {
    pub fn new(global: usize, per_peer: usize) -> Self {
        Self {
            global: Known::new(global),
            peers: BTreeMap::new(),
            per_peer,
        }
    }

    pub fn add_peer(&mut self, id: PeerId) {
        self.peers
            .entry(id)
            .or_insert_with(|| Known::new(self.per_peer));
    }

    pub fn remove_peer(&mut self, id: &PeerId) {
        self.peers.remove(id);
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// An inbound message `body` with BFT message `code` from `from`. The size cap is checked
    /// before the body is decoded (ADR-007 section 5); the decode is strict and the message
    /// must re-encode to the same bytes, so the hash of the bytes is the hash of the message.
    pub fn receive(&mut self, from: &PeerId, code: u64, body: &[u8]) -> Receive {
        let Some(known) = self.peers.get_mut(from) else {
            return Receive::UnknownPeer;
        };
        if !codes::is_bft(code) {
            return Receive::Reject(Offence::Malformed);
        }
        if body.len() > codes::max_size(code) {
            return Receive::Reject(Offence::Oversized);
        }
        let msg = match BftMessage::decode_body(code, body) {
            Ok(m) if m.encode_body() == body => m,
            _ => return Receive::Reject(Offence::Malformed),
        };
        let hash = keccak256(body);
        known.insert(hash);
        if self.global.insert(hash) {
            Receive::Accepted { msg, hash }
        } else {
            Receive::Duplicate
        }
    }

    /// Peers that have not yet seen `hash`, except `except` (the sender). Each is marked as
    /// knowing it, so a second call returns only peers added since: one delivery per peer.
    pub fn targets(&mut self, hash: &B256, except: Option<&PeerId>) -> Vec<PeerId> {
        let mut out = Vec::new();
        for (id, known) in &mut self.peers {
            if Some(id) != except && known.insert(*hash) {
                out.push(*id);
            }
        }
        out
    }

    /// A message we created: remember it globally so an echo is a duplicate, and return every
    /// peer that has not seen it.
    pub fn originate(&mut self, msg: &BftMessage) -> (B256, Vec<PeerId>) {
        let hash = keccak256(msg.encode_body());
        self.global.insert(hash);
        let to = self.targets(&hash, None);
        (hash, to)
    }

    pub fn global_len(&self) -> usize {
        self.global.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B512;
    use xone_primitives::{BlockInfo, SyncInfo, Timeout, Vote};

    fn peer(i: u8) -> PeerId {
        B512::repeat_byte(i)
    }

    fn vote(round: u64) -> BftMessage {
        BftMessage::Vote(Vote {
            proposed_block_info: BlockInfo {
                hash: B256::repeat_byte(1),
                round,
                number: 10,
            },
            signature: vec![7; 65].into(),
            gap_number: 450,
        })
    }

    fn gossip(n: u8) -> Gossip {
        let mut g = Gossip::default();
        for i in 1..=n {
            g.add_peer(peer(i));
        }
        g
    }

    #[test]
    fn each_message_reaches_each_peer_once() {
        let mut g = gossip(5);
        let m = vote(1);
        let body = m.encode_body();
        // peer 1 sends it: accepted, then relayed to the four others, never back to peer 1
        let Receive::Accepted { msg, hash } = g.receive(&peer(1), codes::VOTE, &body) else {
            panic!()
        };
        assert_eq!(msg, m);
        let to = g.targets(&hash, Some(&peer(1)));
        assert_eq!(to, vec![peer(2), peer(3), peer(4), peer(5)]);
        // the same message from peers 2 and 3, and a second relay call: nothing more is sent
        assert_eq!(g.receive(&peer(2), codes::VOTE, &body), Receive::Duplicate);
        assert_eq!(g.receive(&peer(3), codes::VOTE, &body), Receive::Duplicate);
        assert!(g.targets(&hash, None).is_empty());
        // a peer that joins later gets it once
        g.add_peer(peer(6));
        assert_eq!(g.targets(&hash, None), vec![peer(6)]);
        assert!(g.targets(&hash, None).is_empty());
    }

    #[test]
    fn own_message_is_sent_once_and_its_echo_is_a_duplicate() {
        let mut g = gossip(3);
        let m = vote(2);
        let (hash, to) = g.originate(&m);
        assert_eq!(to.len(), 3);
        assert!(g.originate(&m).1.is_empty());
        assert_eq!(
            g.receive(&peer(1), codes::VOTE, &m.encode_body()),
            Receive::Duplicate
        );
        assert_eq!(hash, keccak256(m.encode_body()));
        // a message with another signature is another message
        let other = BftMessage::Timeout(Timeout {
            round: 2,
            signature: vec![1; 65].into(),
            gap_number: 450,
        });
        assert_eq!(g.originate(&other).1.len(), 3);
    }

    #[test]
    fn size_caps_malformed_and_non_canonical_are_scored() {
        let mut g = gossip(1);
        let p = peer(1);
        // over the cap: Oversized, decided before decoding (the body is not even valid RLP)
        assert_eq!(
            g.receive(&p, codes::VOTE, &vec![0xff; codes::MAX_VOTE + 1]),
            Receive::Reject(Offence::Oversized)
        );
        assert_eq!(
            g.receive(&p, codes::TIMEOUT, &vec![0; codes::MAX_TIMEOUT + 1]),
            Receive::Reject(Offence::Oversized)
        );
        assert_eq!(
            g.receive(&p, codes::SYNC_INFO, &vec![0; codes::MAX_SYNC_INFO + 1]),
            Receive::Reject(Offence::Oversized)
        );
        // within the cap but not a message
        for body in [&[][..], &[0x80], &[0xc0], &[0xff; 50]] {
            assert_eq!(
                g.receive(&p, codes::VOTE, body),
                Receive::Reject(Offence::Malformed),
                "{body:?}"
            );
        }
        // a Vote body under the Timeout code, trailing byte, truncation
        let body = vote(3).encode_body();
        assert_eq!(
            g.receive(&p, codes::TIMEOUT, &body),
            Receive::Reject(Offence::Malformed)
        );
        let mut longer = body.clone();
        longer.push(0);
        assert_eq!(
            g.receive(&p, codes::VOTE, &longer),
            Receive::Reject(Offence::Malformed)
        );
        assert_eq!(
            g.receive(&p, codes::VOTE, &body[..body.len() - 1]),
            Receive::Reject(Offence::Malformed)
        );
        // not a BFT code at all
        assert_eq!(
            g.receive(&p, codes::NEW_BLOCK, &body),
            Receive::Reject(Offence::Malformed)
        );
        // nothing rejected was remembered: the good message is still new
        assert!(matches!(
            g.receive(&p, codes::VOTE, &body),
            Receive::Accepted { .. }
        ));
        assert_eq!(
            g.receive(&peer(9), codes::VOTE, &body),
            Receive::UnknownPeer
        );
        // a SyncInfo of 108 + 108 signatures fits its cap
        let sig: alloy_primitives::Bytes = vec![0; 65].into();
        let big = BftMessage::SyncInfo(SyncInfo {
            highest_quorum_cert: xone_primitives::QuorumCert {
                proposed_block_info: BlockInfo {
                    hash: B256::ZERO,
                    round: 1,
                    number: 1,
                },
                signatures: vec![sig.clone(); 108],
                gap_number: 450,
            },
            highest_timeout_cert: xone_primitives::TimeoutCert {
                round: 1,
                signatures: vec![sig; 108],
                gap_number: 450,
            },
        });
        assert!(big.encode_body().len() < codes::MAX_SYNC_INFO);
        assert!(matches!(
            g.receive(&p, codes::SYNC_INFO, &big.encode_body()),
            Receive::Accepted { .. }
        ));
    }

    #[test]
    fn memory_is_bounded() {
        let mut k = Known::new(8);
        for i in 0..1000u32 {
            k.insert(keccak256(i.to_be_bytes()));
            assert!(k.len() <= 8);
        }
        assert!(
            k.contains(&keccak256(999u32.to_be_bytes()))
                && !k.contains(&keccak256(0u32.to_be_bytes()))
        );
        let mut g = Gossip::new(16, 4);
        g.add_peer(peer(1));
        for round in 0..500 {
            let body = vote(round).encode_body();
            g.receive(&peer(1), codes::VOTE, &body);
            assert!(g.global_len() <= 16);
        }
        g.remove_peer(&peer(1));
        assert_eq!(g.peer_count(), 0);
    }
}
