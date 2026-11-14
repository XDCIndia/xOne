//! Static and trusted peers, and the admission rule. Trust is a connection preference only:
//! a trusted peer may take a slot from an ordinary peer and is not banned for a low score. It is
//! never exempt from decoding, size caps, queue bounds or the disconnect that follows an offence
//! (see `peers::Peers`; the xOne lesson: reth banned its own peer and became peerless).

use std::collections::BTreeSet;

use crate::rlpx::{DisconnectReason, PeerId};

#[derive(Debug, Default, Clone)]
pub struct Trust {
    trusted: BTreeSet<PeerId>,
    statics: BTreeSet<PeerId>,
}

impl Trust {
    /// A peer we accept with preference.
    pub fn add_trusted(&mut self, id: PeerId) {
        self.trusted.insert(id);
    }

    /// A peer we also dial and redial; it is trusted as well.
    pub fn add_static(&mut self, id: PeerId) {
        self.statics.insert(id);
    }

    pub fn remove_trusted(&mut self, id: &PeerId) {
        self.trusted.remove(id);
    }

    pub fn remove_static(&mut self, id: &PeerId) {
        self.statics.remove(id);
    }

    pub fn remove(&mut self, id: &PeerId) {
        self.trusted.remove(id);
        self.statics.remove(id);
    }

    pub fn is_trusted(&self, id: &PeerId) -> bool {
        self.trusted.contains(id) || self.statics.contains(id)
    }

    pub fn statics(&self) -> impl Iterator<Item = &PeerId> {
        self.statics.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Accept,
    /// Accept, after disconnecting this ordinary peer to free its slot.
    AcceptEvicting(PeerId),
    Reject(DisconnectReason),
}

/// Decide on a candidate whose handshake has passed. `connected` is oldest first with a trusted
/// flag. Order: duplicates, then bans (not for a trusted candidate), then slots (a trusted
/// candidate evicts the newest ordinary peer; an ordinary one is refused).
pub fn admit(
    trust: &Trust,
    banned: bool,
    connected: &[(PeerId, bool)],
    max_peers: usize,
    candidate: &PeerId,
) -> Admission {
    if connected.iter().any(|(id, _)| id == candidate) {
        return Admission::Reject(DisconnectReason::AlreadyConnected);
    }
    let trusted = trust.is_trusted(candidate);
    if banned && !trusted {
        return Admission::Reject(DisconnectReason::UselessPeer);
    }
    if connected.len() < max_peers {
        return Admission::Accept;
    }
    if trusted {
        if let Some((victim, _)) = connected.iter().rev().find(|(_, t)| !t) {
            return Admission::AcceptEvicting(*victim);
        }
    }
    Admission::Reject(DisconnectReason::TooManyPeers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B512;

    fn id(i: u8) -> PeerId {
        B512::repeat_byte(i)
    }

    #[test]
    fn admission_table() {
        let mut t = Trust::default();
        t.add_trusted(id(10));
        t.add_static(id(11));
        let full = [(id(1), false), (id(2), true), (id(3), false)];
        // free slot
        assert_eq!(admit(&t, false, &full[..2], 3, &id(20)), Admission::Accept);
        // full: an ordinary peer is refused, a trusted or static one evicts the newest ordinary peer
        assert_eq!(
            admit(&t, false, &full, 3, &id(20)),
            Admission::Reject(DisconnectReason::TooManyPeers)
        );
        assert_eq!(
            admit(&t, false, &full, 3, &id(10)),
            Admission::AcceptEvicting(id(3))
        );
        assert_eq!(
            admit(&t, false, &full, 3, &id(11)),
            Admission::AcceptEvicting(id(3))
        );
        // full of trusted peers: nobody to evict
        let all_trusted = [(id(2), true), (id(4), true)];
        assert_eq!(
            admit(&t, false, &all_trusted, 2, &id(10)),
            Admission::Reject(DisconnectReason::TooManyPeers)
        );
        // a ban refuses an ordinary peer; a trusted peer keeps its preference
        assert_eq!(
            admit(&t, true, &[], 3, &id(20)),
            Admission::Reject(DisconnectReason::UselessPeer)
        );
        assert_eq!(admit(&t, true, &[], 3, &id(10)), Admission::Accept);
        // a duplicate is refused first, trusted or not
        assert_eq!(
            admit(&t, false, &full, 3, &id(2)),
            Admission::Reject(DisconnectReason::AlreadyConnected)
        );
        t.remove(&id(10));
        assert!(!t.is_trusted(&id(10)) && t.is_trusted(&id(11)));
        assert_eq!(t.statics().count(), 1);
    }
}
