/// Token bucket with a caller-supplied clock (milliseconds).
///
/// Tokens are the unit of the caller's choice: bytes for logs, cost units for
/// RPC. Internally scaled by 1000 so refill is exact for any rate with no
/// float and no lost remainder.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    /// Capacity in milli-tokens.
    cap: u64,
    /// Milli-tokens added per millisecond, equal to tokens per second.
    rate: u64,
    level: u64,
    last_ms: u64,
}

impl TokenBucket {
    /// Starts full. `burst` is the capacity, `per_sec` the sustained rate.
    pub fn new(burst: u64, per_sec: u64, now_ms: u64) -> Self {
        let cap = burst.saturating_mul(1000);
        Self {
            cap,
            rate: per_sec,
            level: cap,
            last_ms: now_ms,
        }
    }

    /// Takes `cost` tokens if available. A cost above `burst` never succeeds.
    pub fn try_take(&mut self, now_ms: u64, cost: u64) -> bool {
        // A clock that goes backwards adds nothing and is not adopted.
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        self.level = self
            .level
            .saturating_add(dt.saturating_mul(self.rate))
            .min(self.cap);
        let need = cost.saturating_mul(1000);
        if need <= self.level {
            self.level -= need;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_second_bounded_under_flood() {
        // 1,000 B/s sustained, 4,000 B burst; a 100 B line is offered every ms for 60 s.
        let mut b = TokenBucket::new(4_000, 1_000, 0);
        let mut bytes = 0u64;
        for t in 0..60_000 {
            if b.try_take(t, 100) {
                bytes += 100;
            }
        }
        // Upper bound: burst plus rate times elapsed seconds.
        assert!(bytes <= 4_000 + 1_000 * 60, "{bytes}");
        // And the limiter is not starving: within one line of the bound.
        assert!(bytes >= 4_000 + 1_000 * 60 - 100, "{bytes}");
    }

    #[test]
    fn no_remainder_lost_at_fine_grain() {
        // 3 tokens/s polled every ms: 1 token per 333.3 ms must still yield 3 per second.
        let mut b = TokenBucket::new(10, 3, 0);
        assert!(b.try_take(0, 10));
        let n = (1..=10_000).filter(|&t| b.try_take(t, 1)).count();
        assert_eq!(n, 30);
    }

    #[test]
    fn oversize_cost_never_passes_and_clock_back_is_safe() {
        let mut b = TokenBucket::new(10, 10, 1_000);
        assert!(!b.try_take(1_000_000, 11));
        assert!(b.try_take(5, 10)); // earlier time: no refill, but a full bucket pays
        assert!(!b.try_take(0, 1));
    }
}
