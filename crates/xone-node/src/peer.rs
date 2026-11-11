/// Why a message counts against a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offence {
    /// Does not decode.
    Malformed,
    /// Decodes, fails a validity rule (bad signature, bad header).
    Invalid,
    /// Above the size cap of its message class.
    Oversized,
    /// Above its queue or rate budget.
    RateExceeded,
}

/// Typed reason to drop a peer. The ban lasts until `ban_until_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disconnect {
    pub reason: Offence,
    pub ban_until_ms: u64,
}

/// Points per offence and the ban rule. All values are config with safe defaults.
#[derive(Clone, Copy, Debug)]
pub struct PeerLimits {
    pub malformed: u32,
    pub invalid: u32,
    pub oversized: u32,
    pub rate_exceeded: u32,
    /// Score at which the peer is disconnected and banned.
    pub threshold: u32,
    /// Points forgiven per minute of good behaviour.
    pub decay_per_min: u32,
    pub ban_ms: u64,
}

impl Default for PeerLimits {
    /// Proposals (ADR-007 section 8): 10 malformed in a short window bans;
    /// one oversized message bans at once.
    fn default() -> Self {
        Self {
            malformed: 10,
            invalid: 25,
            oversized: 100,
            rate_exceeded: 5,
            threshold: 100,
            decay_per_min: 10,
            ban_ms: 30 * 60 * 1000,
        }
    }
}

/// Score accounting for one peer.
///
/// ponytail: fixed ban time, no repeat-offender escalation, and no ban table;
/// the peer manager keeps `ban_until_ms` per identity and may exempt trusted peers.
#[derive(Clone, Debug)]
pub struct PeerBudget {
    limits: PeerLimits,
    /// Score scaled by 60_000 so decay is exact per millisecond.
    score: u64,
    last_ms: u64,
}

const SCALE: u64 = 60_000;

impl PeerBudget {
    pub fn new(limits: PeerLimits, now_ms: u64) -> Self {
        Self {
            limits,
            score: 0,
            last_ms: now_ms,
        }
    }

    /// Records one offence. `Some` means: disconnect now and ban.
    pub fn record(&mut self, now_ms: u64, offence: Offence) -> Option<Disconnect> {
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        self.score = self
            .score
            .saturating_sub(dt.saturating_mul(self.limits.decay_per_min as u64));
        let l = &self.limits;
        let pts = match offence {
            Offence::Malformed => l.malformed,
            Offence::Invalid => l.invalid,
            Offence::Oversized => l.oversized,
            Offence::RateExceeded => l.rate_exceeded,
        };
        self.score = self.score.saturating_add(pts as u64 * SCALE);
        (self.score >= l.threshold as u64 * SCALE).then(|| Disconnect {
            reason: offence,
            ban_until_ms: self.last_ms.saturating_add(l.ban_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bans_after_n_malformed() {
        // Same millisecond: no decay, so the tenth message reaches the threshold.
        let mut p = PeerBudget::new(PeerLimits::default(), 0);
        for _ in 0..9 {
            assert_eq!(p.record(1_000, Offence::Malformed), None);
        }
        let d = p.record(1_000, Offence::Malformed).unwrap();
        assert_eq!(d.reason, Offence::Malformed);
        assert_eq!(d.ban_until_ms, 1_000 + 30 * 60 * 1000);
    }

    #[test]
    fn oversized_bans_at_once() {
        let mut p = PeerBudget::new(PeerLimits::default(), 0);
        assert_eq!(
            p.record(0, Offence::Oversized).unwrap().reason,
            Offence::Oversized
        );
    }

    #[test]
    fn slow_offender_decays_and_is_not_banned() {
        // One malformed message per 2 minutes: 10 points added, 20 forgiven.
        let mut p = PeerBudget::new(PeerLimits::default(), 0);
        for i in 0..1_000u64 {
            assert_eq!(p.record(i * 120_000, Offence::Malformed), None);
        }
    }

    #[test]
    fn mixed_offences_add_up() {
        let mut p = PeerBudget::new(PeerLimits::default(), 0);
        for _ in 0..3 {
            assert_eq!(p.record(0, Offence::Invalid), None); // 75
        }
        assert_eq!(p.record(0, Offence::Malformed), None); // 85
        assert!(p.record(0, Offence::Invalid).is_some()); // 110
    }
}
