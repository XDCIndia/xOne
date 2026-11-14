//! Slow-block log (`--debug.logslowblock`, geth-XDC `core/blockchain_stats.go` `logSlow`): one
//! JSON line per imported block whose processing took at least the threshold, with the time of
//! each phase. geth semantics: a negative threshold is off (the default), 0 logs every block.

use std::time::Duration;

use serde_json::json;

/// The time of each phase of one block import ([`crate::services::core::Core::import`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phases {
    /// Header and QC verification (`NodeCheck`).
    pub verify: Duration,
    /// Waiting for the sender recovery of the body.
    pub senders: Duration,
    /// Transaction execution and the state root (`execute_verified`).
    pub execute: Duration,
    /// The whole `Chain::insert`: verification, execution and the store commit.
    pub insert: Duration,
    /// The history record, consensus rows, subscriptions and checkpoint start after the commit.
    pub history: Duration,
    /// Start of the import to the end.
    pub total: Duration,
}

impl Phases {
    /// The store commit: what `Chain::insert` spent outside verification and execution.
    pub fn commit(&self) -> Duration {
        self.insert
            .saturating_sub(self.verify)
            .saturating_sub(self.senders)
            .saturating_sub(self.execute)
    }
}

/// `true` when a block of `total` must be logged: the threshold is set and `total` reaches it.
pub fn is_slow(threshold: Option<Duration>, total: Duration) -> bool {
    threshold.is_some_and(|t| total >= t)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The log line (geth `slowBlockLog`: level, msg, block, timing, throughput; the timing fields
/// are the xOne phases).
pub fn line(number: u64, hash: &str, gas_used: u64, tx_count: usize, p: &Phases) -> String {
    let secs = p.total.as_secs_f64();
    #[allow(clippy::as_conversions)] // gas to f64 for a rate shown with 2 decimals
    let mgas = if secs > 0.0 {
        gas_used as f64 / 1e6 / secs
    } else {
        0.0
    };
    json!({
        "level": "warn",
        "msg": "Slow block",
        "block": {"number": number, "hash": hash, "gas_used": gas_used, "tx_count": tx_count},
        "timing": {
            "verify_ms": ms(p.verify),
            "senders_ms": ms(p.senders),
            "execution_ms": ms(p.execute),
            "commit_ms": ms(p.commit()),
            "history_ms": ms(p.history),
            "total_ms": ms(p.total),
        },
        "throughput": {"mgas_per_sec": mgas},
    })
    .to_string()
}

/// A Go `time.ParseDuration` value (`1s`, `500ms`, `1m30s`, `1.5s`, `0`, `-1ns`): `None` when
/// negative (off), the duration otherwise.
pub fn parse_duration(s: &str) -> Result<Option<Duration>, String> {
    let bad = || format!("{s:?} is not a duration (examples: 1s, 500ms, 0, -1ns)");
    let (neg, mut rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if rest == "0" {
        return Ok((!neg).then_some(Duration::ZERO));
    }
    if rest.is_empty() {
        return Err(bad());
    }
    let mut nanos: u128 = 0;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or_else(bad)?;
        let (num, tail) = rest.split_at(num_end);
        let unit_end = tail
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(tail.len());
        let (unit, next) = tail.split_at(unit_end);
        let scale: u128 = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(bad()),
        };
        let (int, frac) = num.split_once('.').unwrap_or((num, ""));
        if int.is_empty() && frac.is_empty() {
            return Err(bad());
        }
        let int: u128 = if int.is_empty() {
            0
        } else {
            int.parse().map_err(|_| bad())?
        };
        let mut part = int.checked_mul(scale).ok_or_else(bad)?;
        let mut div: u128 = 1;
        for c in frac.chars().take(18) {
            let d = u128::from(c.to_digit(10).ok_or_else(bad)?);
            div = div.saturating_mul(10);
            part = part.saturating_add(d.saturating_mul(scale) / div);
        }
        nanos = nanos.checked_add(part).ok_or_else(bad)?;
        rest = next;
    }
    if neg {
        return Ok(None);
    }
    let secs = u64::try_from(nanos / 1_000_000_000).map_err(|_| bad())?;
    let sub = u32::try_from(nanos % 1_000_000_000).map_err(|_| bad())?;
    Ok(Some(Duration::new(secs, sub)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_durations() {
        let d = |s| parse_duration(s);
        assert_eq!(d("1s"), Ok(Some(Duration::from_secs(1))));
        assert_eq!(d("500ms"), Ok(Some(Duration::from_millis(500))));
        assert_eq!(d("1.5s"), Ok(Some(Duration::from_millis(1500))));
        assert_eq!(d("1m30s"), Ok(Some(Duration::from_secs(90))));
        assert_eq!(d("250us"), Ok(Some(Duration::from_micros(250))));
        assert_eq!(d("0"), Ok(Some(Duration::ZERO)));
        assert_eq!(d("0s"), Ok(Some(Duration::ZERO)));
        assert_eq!(d("-1ns"), Ok(None));
        assert_eq!(d("-1s"), Ok(None));
        for bad in ["", "1", "s", "1x", "-", "1.2.3s", "ms5"] {
            assert!(d(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn threshold() {
        let ms = Duration::from_millis;
        assert!(!is_slow(None, ms(10_000)));
        assert!(is_slow(Some(Duration::ZERO), Duration::ZERO));
        assert!(is_slow(Some(ms(1000)), ms(1000)));
        assert!(!is_slow(Some(ms(1000)), ms(999)));
    }

    #[test]
    fn line_has_every_phase() {
        let ms = Duration::from_millis;
        let p = Phases {
            verify: ms(10),
            senders: ms(5),
            execute: ms(800),
            insert: ms(900),
            history: ms(100),
            total: ms(1000),
        };
        assert_eq!(p.commit(), ms(85));
        let v: serde_json::Value = serde_json::from_str(&line(7, "0xab", 30_000_000, 12, &p))
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(v["msg"], "Slow block");
        assert_eq!(v["block"]["number"], 7);
        assert_eq!(v["block"]["tx_count"], 12);
        for k in [
            "verify_ms",
            "senders_ms",
            "execution_ms",
            "commit_ms",
            "history_ms",
            "total_ms",
        ] {
            assert!(v["timing"][k].is_number(), "{k}");
        }
        assert_eq!(v["timing"]["commit_ms"], 85.0);
        assert_eq!(v["throughput"]["mgas_per_sec"], 30.0);
    }
}
