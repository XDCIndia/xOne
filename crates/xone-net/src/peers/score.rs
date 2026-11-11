//! Score accounting per peer identity, on top of `xone_node::PeerBudget` (ADR-007 section 5).
//! A peer that crosses the threshold is disconnected; unless it is trusted it is also banned.
//! A ban lasts until a time and survives a reconnect, because it is keyed by node id.

use std::collections::HashMap;

use xone_node::{Offence, PeerBudget, PeerLimits};

use crate::rlpx::PeerId;

/// Peers and bans remembered. At the limit the oldest ban expiring first is dropped and the
/// budgets are cleared. ponytail: a real LRU when peer churn makes this matter.
const MAX_TRACKED: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Disconnect now. `ban_until_ms` is `None` for a trusted peer: it may reconnect at once.
    Disconnect {
        reason: Offence,
        ban_until_ms: Option<u64>,
    },
}

#[derive(Debug)]
pub struct Scoreboard {
    limits: PeerLimits,
    budgets: HashMap<PeerId, PeerBudget>,
    bans: HashMap<PeerId, u64>,
}

impl Scoreboard {
    pub fn new(limits: PeerLimits) -> Self {
        Self {
            limits,
            budgets: HashMap::new(),
            bans: HashMap::new(),
        }
    }

    /// Count one offence. `trusted` only decides whether a ban follows; the score and the
    /// disconnect are the same for every peer.
    pub fn record(&mut self, now_ms: u64, id: &PeerId, offence: Offence, trusted: bool) -> Verdict {
        if self.budgets.len() >= MAX_TRACKED && !self.budgets.contains_key(id) {
            self.budgets.clear();
        }
        let limits = self.limits;
        let budget = self
            .budgets
            .entry(*id)
            .or_insert_with(|| PeerBudget::new(limits, now_ms));
        let Some(d) = budget.record(now_ms, offence) else {
            return Verdict::Keep;
        };
        // a fresh start after a disconnect, banned or not
        self.budgets.remove(id);
        if trusted {
            return Verdict::Disconnect {
                reason: d.reason,
                ban_until_ms: None,
            };
        }
        if self.bans.len() >= MAX_TRACKED {
            self.bans.retain(|_, until| *until > now_ms);
            if self.bans.len() >= MAX_TRACKED {
                let first = self.bans.iter().min_by_key(|(_, u)| **u).map(|(k, _)| *k);
                if let Some(k) = first {
                    self.bans.remove(&k);
                }
            }
        }
        self.bans.insert(*id, d.ban_until_ms);
        Verdict::Disconnect {
            reason: d.reason,
            ban_until_ms: Some(d.ban_until_ms),
        }
    }

    pub fn is_banned(&mut self, id: &PeerId, now_ms: u64) -> bool {
        match self.bans.get(id) {
            Some(until) if *until > now_ms => true,
            Some(_) => {
                self.bans.remove(id);
                false
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B512;

    fn id(i: u8) -> PeerId {
        B512::repeat_byte(i)
    }

    #[test]
    fn threshold_disconnects_and_bans_until_expiry() {
        let mut s = Scoreboard::new(PeerLimits::default());
        for _ in 0..9 {
            assert_eq!(
                s.record(1_000, &id(1), Offence::Malformed, false),
                Verdict::Keep
            );
        }
        let v = s.record(1_000, &id(1), Offence::Malformed, false);
        let until = 1_000 + 30 * 60 * 1000;
        assert_eq!(
            v,
            Verdict::Disconnect {
                reason: Offence::Malformed,
                ban_until_ms: Some(until)
            }
        );
        assert!(s.is_banned(&id(1), 1_001) && s.is_banned(&id(1), until - 1));
        assert!(!s.is_banned(&id(1), until));
        assert!(!s.is_banned(&id(2), 1_001));
    }

    #[test]
    fn oversized_is_immediate_and_trusted_is_disconnected_but_not_banned() {
        let mut s = Scoreboard::new(PeerLimits::default());
        assert!(matches!(
            s.record(0, &id(1), Offence::Oversized, false),
            Verdict::Disconnect {
                ban_until_ms: Some(_),
                ..
            }
        ));
        // the same offence from a trusted peer: the same disconnect, no ban
        assert_eq!(
            s.record(0, &id(2), Offence::Oversized, true),
            Verdict::Disconnect {
                reason: Offence::Oversized,
                ban_until_ms: None
            }
        );
        assert!(!s.is_banned(&id(2), 1));
        // and it starts again from zero
        assert_eq!(s.record(0, &id(2), Offence::Malformed, true), Verdict::Keep);
    }
}
