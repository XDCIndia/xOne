# XDPoS v2 parameters are selected by round

## Rule

`XDPoS.V2` has a switch block, a switch epoch and `AllConfigs`, a map from a switch round to a `V2Config`. The entry in force at round `r` is the one with the highest key `<= r`. geth builds a descending key index once (`BuildConfigIndex`) and `Config(round)` returns the first key `<= round`; with no index it returns `CurrentConfig`, which is the round-0 entry.

The `SwitchRound` field inside an entry is unset on several entries. The map key is the authority, not the field.

The exporter emits every `V2Config` field, zero values included: `switchRound`, `minePeriod`, `timeoutSyncThreshold`, `timeoutPeriod`, `certificateThreshold`, `maxMasternodes`, `maxProtectorNodes`, `maxObserverNodes`, `masternodeReward`, `protectorReward`, `observerReward`, `minimumMinerBlockPerEpoch`, `limitPenaltyEpoch`, `minimumSigningTx`, `expTimeoutConfig.{base,maxExponent}`. Entries are listed in numeric round order. Rust rejects unknown keys, so a field added to geth must be added to the loader on purpose.

`XDPoS.MaxMasternodesV2` is separate and is nonzero only on devnet551.

## Source

- `params/config.go:2288-2297` `XDPoSConfig`; `:2392-2398` `V2`.
- `params/config.go:2405-2418` `BuildConfigIndex`; `:2425-2438` `V2.Config`; `:2442` `UpdateConfig`.
- `params/config.go:2465-2490` `V2Config`; `:2493-2496` `ExpTimeoutConfig`.
- Per-network values: `MainnetV2Configs` (`:2501`), `ApothemV2Configs` (`:2563`), `DevnetV2Configs` (`:2646`), `Devnet551V2Configs` (`:2677`); the exported values are authoritative.
- `core/genesis.go:932-987` fills missing round entries from defaults when a registered network is loaded from a genesis file. The Rust table is complete and does not default.

## Networks

All four.

## Rust

- `V2::configs`, `V2::config_at_round`, `V2Config`, `ExpTimeoutConfig`.
- Tests: `config_at_round_picks_highest_switch_round_not_above`, `xdpos_snapshot` (every round entry of every network).

## Legacy

geth-XDC `CurrentConfig` mutates through `UpdateConfig`; the Rust model is a pure lookup by round. Behaviour of `UpdateConfig` is not part of this page.
