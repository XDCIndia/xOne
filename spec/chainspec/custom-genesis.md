# Custom genesis and the net2040 profile

Crate: `xone-chainspec` (`ChainSpec::from_genesis_json`, `Network::Net2040`). Reference: geth-XDC at `conformance/PIN`.

`from_genesis_json` turns the `config` block of a genesis file into a `ChainSpec`: the fork rows, `XDPoS` and `XDPoS.v2` (`allConfigs` by round, `switchBlock`, `switchEpoch`), the genesis state root and the genesis hash computed from the file. It goes through the same loader as the generated table, so a row the crate does not model is `UnmodelledFork` and a value that is not a block number is `InvalidValue`.

Rules:

- `tipUpgradeRewardBlock` is the old name of `tipFixedRewardBlock` (`params/config.go:2337-2380`); both keys with different values are an error.
- A required row that is absent or null is `MissingFork`. Required are the rows geth falls back on a per-chain-id global for when they are absent (`common.CopyXDCConstants`, `common/types.go:210-310`), which xOne has no table for: `homestead`, `eip150`, `eip155`, `eip158`, `byzantium`, `constantinople`, `istanbul`, `berlin`, `london`, `tip2019`, `tipSigning`, `tipRandomize`, `tipNoHalvingMNReward`, `tipXDCX`, `tipXDCXCancellationFee`, `tipTRC21Fee`, `gas50x`, `tipXDCXMinerDisable`, `tipXDCXReceiverDisable`. Every other row absent is geth `nil`: never active.
- The `effective` rows are those of the generated table: the row itself, except `istanbul` and `petersburg`, which also fire at `TIPXDCXCancellationFee` (a `nil` Petersburg row means Constantinople).
- `XDPoS.SkipV1Validation = true` is `Unsupported`: xOne never skips a check.
- Nothing is read from the chain id.

`net2040` is built in from `genesis/net2040.json` (byte-exact from the net2040 network) and from the row of `generated/forks.json` that `conformance/exporter` writes from that file. Test: the loader on that file equals the built-in spec, and the genesis hash equals block 0 of the archive.
