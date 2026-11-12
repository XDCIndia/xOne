# Slot timing (XB-23)

## Rule

- **V1 time:** `max(parent + period, now)`. **V2 time:** `max(parent + minePeriod(round), now)`, the config row of the block's round (the verifier reads the same row).
- **V2 may build** once `now - parent.time >= minePeriod` (whole seconds). **V1 does not build in the parent's second.**
- **Timer** (`getResetTimeXDC`): wait `head.time * 1000 + period * 1000 - now_ms`; a wait above one period, or at most zero, is one period.
- **V1 release:** a sealed block is sent at its own time; a difficulty-1 block (also a lone masternode's) adds `random(0, (len/2 + 1) * 500 ms)`. The caller owns the randomness.
- **BFT timers** are the machine's (`v2::timer`); the producer reads only the `mine_period` of `Effect::AdvanceRound`.

## Source

`xdpos.go:1489-1492`, `:1834-1840`, `:2780`; `engine_v2/engine.go:755-758`, `:888-897`; `miner/xdpos_worker.go:274-290`, `:584`, `:787-797`.

## Rust

`xone_builder::producer::timing::{v1_block_time, v2_block_time, v2_period_elapsed, v1_parent_second_passed, reset_time, v1_release_delay}`.

## Vectors

`tests/producer_schedule.rs`: 50 consecutive blocks of each run (Apothem V1 replay, Apothem V2 replay, Apothem head, net2040 head) have the producer's timestamp for their own time and never come before their period (41 to 43 of 49 are exactly on the slot); the mine period and timeout period of rounds across the schedules of Apothem and mainnet equal the pinned `params` after `BuildConfigIndex` (`oracle.json`); the `getResetTimeXDC` and release-delay cases by hand. Mainnet has no archive vector here (no access): its rows are checked against `params` only.
