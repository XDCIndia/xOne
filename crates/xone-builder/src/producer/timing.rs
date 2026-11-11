//! Slot timing (`spec/producer/timing.md`): when a block may be built, which timestamp it carries and
//! how long the worker waits. No clock is read here: the caller passes `now`.

use std::time::Duration;

/// geth `wiggleTime`, `consensus/XDPoS/xdpos.go:2780`.
pub const WIGGLE_MS: u64 = 500;
/// geth `signingFreshnessBound`, `miner/xdpos_worker.go:1192`: `2 * MergeSignRange`.
pub const SIGN_FRESHNESS: u64 = 30;

/// V1 `Prepare` (`consensus/XDPoS/xdpos.go:1487-1491`): `parent.Time + Period`, or now once that is past.
pub fn v1_block_time(parent_time: u64, period: u64, now: u64) -> u64 {
    parent_time.saturating_add(period).max(now)
}

/// V2 `Prepare` (`engine_v2/engine.go:747-748`): `parent.Time + MinePeriod(round)`, or now once that is
/// past. The period is the config row of the block's round (the verifier reads the same row).
pub fn v2_block_time(parent_time: u64, mine_period: u64, now: u64) -> u64 {
    parent_time.saturating_add(mine_period).max(now)
}

/// V2 `YourTurn` (`engine_v2/engine.go:895-897`): the mine period has passed since the parent. Whole seconds.
pub fn v2_period_elapsed(parent_time: u64, mine_period: u64, now: u64) -> bool {
    now.saturating_sub(parent_time) >= mine_period
}

/// V1 `commitV1Work` (`miner/xdpos_worker.go:540-556`): the worker does not build in the parent's second.
pub fn v1_parent_second_passed(parent_time: u64, now: u64) -> bool {
    now > parent_time
}

/// geth `getResetTimeXDC` (`miner/xdpos_worker.go:274-290`): the wait before the next attempt, from the
/// head's timestamp, the mine period and now in milliseconds. A wait above one period or at most zero is
/// one period.
pub fn reset_time(head_time: u64, mine_period: u64, now_ms: u64) -> Duration {
    let period_ms = i128::from(mine_period) * 1000;
    let wait = i128::from(head_time) * 1000 + period_ms - i128::from(now_ms);
    let ms = if wait > period_ms || wait <= 0 { period_ms } else { wait };
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// V1 `Seal` (`xdpos.go:1830-1840`): the wait before a sealed block is released. The block's own time
/// less now; a block of difficulty 1 (the "no turn" difficulty, which a lone masternode also carries)
/// adds a random part of `(masternodes / 2 + 1) * 500 ms`. `jitter` is that random draw, `0..wiggle`:
/// the caller owns the randomness.
pub fn v1_release_delay(
    block_time: u64,
    now_ms: u64,
    difficulty: u64,
    masternodes: usize,
    jitter: u64,
) -> Duration {
    let base = i128::from(block_time) * 1000 - i128::from(now_ms);
    let wiggle = (u64::try_from(masternodes).unwrap_or(0) / 2 + 1) * WIGGLE_MS;
    let extra = if difficulty == 1 { jitter % wiggle } else { 0 };
    let ms = (base + i128::from(extra)).max(0);
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}
