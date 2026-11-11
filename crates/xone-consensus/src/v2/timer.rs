//! The round timer: geth `ExpCountDown` (`common/countdown/countdown.go`) and the way the V2 engine
//! drives it (`engine.go:171-190` construction, `:3220-3250` `setNewRound`, `:370-570` `initial`).
//!
//! The timer has no clock. [`Timer::reset`] and [`Timer::fire`] return the delay the driver must wait;
//! each returns the same `token`, and a fired timer whose token is not the latest is stale and ignored,
//! as geth stops the old `time.AfterFunc` on every reset (`countdown.go:79-82`).
//!
//! The delay is `period * base^min(cur - qc, max_exp)` (`countdown.go:90-99`), with `cur` the round
//! the timer was reset for and `qc` the round of the highest QC at that moment. When the delay passes
//! the timer restarts itself with `cur + 1` and the same `qc` (`countdown.go:139-145`), so a round that
//! keeps timing out waits one step longer each time, up to `max_exp` steps.

use std::time::Duration;

use xone_chainspec::V2Config;

#[derive(Debug, Clone)]
pub struct Timer {
    period_ns: u64,
    base: f64,
    max_exp: u8,
    cur: u64,
    qc: u64,
    token: u64,
}

/// Go `math.Pow(x, float64(n))` for `x >= 1` and a whole `n` (`math/pow.go`): the same square-and-multiply
/// on the mantissa and exponent, so the last bit agrees with geth (the schedule is `time.Duration` of
/// the product, which truncates).
fn pow(x: f64, n: u64) -> f64 {
    if n == 0 || x == 1.0 {
        return 1.0;
    }
    if n == 1 {
        return x;
    }
    let bits = x.to_bits();
    // x is finite and at least 1, so it is normal: frexp is bit work
    #[allow(clippy::as_conversions)] // 11-bit field
    let mut xe = ((bits >> 52) & 0x7ff) as i64 - 1022;
    let mut x1 = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    let (mut a1, mut ae, mut i) = (1.0_f64, 0_i64, n);
    while i != 0 {
        if i & 1 == 1 {
            a1 *= x1;
            ae += xe;
        }
        x1 *= x1;
        xe <<= 1;
        if x1 < 0.5 {
            x1 += x1;
            xe -= 1;
        }
        i >>= 1;
    }
    // ldexp(a1, ae); a1 is in [0.25, 1), the exponent of any sane schedule is far inside the range
    if ae > 1023 {
        return f64::INFINITY;
    }
    #[allow(clippy::as_conversions)] // ae is in -1022..=1023 after the clamp
    let scale = f64::from_bits(((ae.max(-1022) + 1023) as u64) << 52);
    a1 * scale
}

impl Timer {
    /// geth `createEngine` (`engine.go:171-195`): the timer of the config at round 0. An unset base
    /// is 2 and an unset max exponent is 6; `NewExpCountDown` then lifts a base below 1 to 1.
    pub fn new(row: &V2Config) -> Self {
        let base = if row.exp_timeout.base > 0.0 {
            row.exp_timeout.base
        } else {
            2.0
        };
        Self {
            period_ns: row.timeout_period.saturating_mul(1_000_000_000),
            base: base.max(1.0),
            max_exp: if row.exp_timeout.max_exponent > 0 {
                row.exp_timeout.max_exponent
            } else {
                6
            },
            cur: 0,
            qc: 0,
            token: 0,
        }
    }

    /// geth `SetParams` as `setNewRound` calls it (`engine.go:3231-3237`, `countdown.go:60-72`): a base
    /// below 1 is ignored and the old base stays; the max exponent is always taken.
    pub fn set_params(&mut self, row: &V2Config) {
        self.period_ns = row.timeout_period.saturating_mul(1_000_000_000);
        if row.exp_timeout.base >= 1.0 {
            self.base = row.exp_timeout.base;
        }
        self.max_exp = row.exp_timeout.max_exponent;
    }

    /// geth `Reset(chain, cur, qc)`: stop the old timer and start one. Returns `(token, delay)`.
    pub fn reset(&mut self, cur: u64, qc: u64) -> (u64, Duration) {
        self.cur = cur;
        self.qc = qc;
        self.token += 1;
        (self.token, self.delay())
    }

    /// geth `onTimeout` after the callback ran (`countdown.go:139-145`): the timer restarts with
    /// `cur + 1`.
    pub fn fire(&mut self) -> (u64, Duration) {
        self.reset(self.cur.wrapping_add(1), self.qc)
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    /// geth `GetTimeout`: `Duration(float64(period) * Pow(base, float64(diff)))`. The difference is the
    /// unsigned subtraction read as `int`: a QC above the round gives a negative number, which is 0.
    pub fn delay(&self) -> Duration {
        let diff = self.cur.wrapping_sub(self.qc);
        let diff = if diff >> 63 == 1 {
            0
        } else {
            diff.min(u64::from(self.max_exp))
        };
        #[allow(clippy::as_conversions)]
        // float to integer truncates toward zero and saturates, as Go does for these magnitudes
        let ns = (self.period_ns as f64 * pow(self.base, diff)) as u64;
        Duration::from_nanos(ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xone_chainspec::ExpTimeoutConfig;

    fn row(period: u64, base: f64, max_exponent: u8) -> V2Config {
        V2Config {
            timeout_period: period,
            exp_timeout: ExpTimeoutConfig { base, max_exponent },
            ..V2Config::default()
        }
    }

    #[test]
    fn flat_and_exponential() {
        let mut t = Timer::new(&row(10, 1.0, 0));
        t.set_params(&row(10, 2.0, 3));
        assert_eq!(t.reset(5, 5).1, Duration::from_secs(10));
        assert_eq!(t.reset(7, 5).1, Duration::from_secs(40));
        // fire: cur + 1, and the exponent stops at 3
        assert_eq!(t.fire().1, Duration::from_secs(80));
        assert_eq!(t.fire().1, Duration::from_secs(80));
        // a QC above the round is a negative difference
        assert_eq!(t.reset(3, 9).1, Duration::from_secs(10));
    }

    #[test]
    fn an_old_token_is_stale() {
        let mut t = Timer::new(&row(10, 1.0, 0));
        let (a, _) = t.reset(1, 0);
        let (b, _) = t.fire();
        assert_ne!(a, b);
        assert_eq!(t.token(), b);
    }
}
