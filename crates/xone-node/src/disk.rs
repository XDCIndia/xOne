/// What the import pipeline does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Continue,
    /// Stop block import. Raise one alarm on the transition into this state.
    StopImportAlarm,
    /// Exit cleanly: free space is below what a commit and the safety record need.
    Exit,
}

/// Free-space thresholds in bytes. Pure: the caller measures free bytes.
///
/// `exit_below < stop_below < resume_above` is required; `new` returns `None`
/// otherwise, so a mis-set config is refused at start rather than flapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskWatermark {
    exit_below: u64,
    stop_below: u64,
    resume_above: u64,
}

impl DiskWatermark {
    pub fn new(exit_below: u64, stop_below: u64, resume_above: u64) -> Option<Self> {
        (exit_below < stop_below && stop_below < resume_above).then_some(Self {
            exit_below,
            stop_below,
            resume_above,
        })
    }

    pub fn stop_below(&self) -> u64 {
        self.stop_below
    }

    pub fn exit_below(&self) -> u64 {
        self.exit_below
    }

    /// Thresholds for a filesystem of `total` bytes: each is the larger of its percent of `total`
    /// and its byte floor (ADR-007 section 7). `None` unless `exit < stop < resume`: a misordered
    /// config is refused at start, there is no switch to turn the watermark off.
    pub fn from_limits(total: u64, l: &crate::config::Limits) -> Option<Self> {
        let at = |pct: u8, min: u64| {
            u64::try_from(u128::from(total) * u128::from(pct) / 100)
                .unwrap_or(u64::MAX)
                .max(min)
        };
        Self::new(
            at(l.disk_exit_pct, l.disk_exit_min_bytes),
            at(l.disk_stop_pct, l.disk_stop_min_bytes),
            at(l.disk_resume_pct, l.disk_resume_min_bytes),
        )
    }

    /// Next action from the free bytes and the previous action.
    ///
    /// Hysteresis: once stopped, import resumes only above `resume_above`.
    /// `Exit` is terminal. Alarm once per transition: alarm when the result is
    /// `StopImportAlarm` and `prev` was not.
    pub fn decide(&self, free: u64, prev: Action) -> Action {
        if prev == Action::Exit || free < self.exit_below {
            return Action::Exit;
        }
        match prev {
            Action::StopImportAlarm if free <= self.resume_above => Action::StopImportAlarm,
            _ if free < self.stop_below => Action::StopImportAlarm,
            _ => Action::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    fn wm() -> DiskWatermark {
        DiskWatermark::new(20, 100, 150).unwrap()
    }

    #[test]
    fn rejects_misordered_thresholds() {
        assert!(DiskWatermark::new(100, 100, 150).is_none());
        assert!(DiskWatermark::new(20, 100, 100).is_none());
    }

    #[test]
    fn levels() {
        assert_eq!(wm().decide(500, Continue), Continue);
        assert_eq!(wm().decide(99, Continue), StopImportAlarm);
        assert_eq!(wm().decide(19, StopImportAlarm), Exit);
        assert_eq!(wm().decide(19, Continue), Exit);
    }

    #[test]
    fn no_flapping_around_the_stop_line() {
        // Free space jitters across stop_below: one alarm, no resume until resume_above.
        let mut prev = Continue;
        let mut alarms = 0;
        for free in [120, 101, 99, 101, 99, 120, 149, 150, 99, 140] {
            let next = wm().decide(free, prev);
            if next == StopImportAlarm && prev != StopImportAlarm {
                alarms += 1;
            }
            prev = next;
        }
        // 99 -> alarm 1; stays stopped through 150 (not above); 151 is needed to resume.
        assert_eq!(alarms, 1);
        assert_eq!(prev, StopImportAlarm);
        assert_eq!(wm().decide(151, prev), Continue);
    }

    #[test]
    fn exit_is_terminal() {
        assert_eq!(wm().decide(10_000, Exit), Exit);
    }
}
