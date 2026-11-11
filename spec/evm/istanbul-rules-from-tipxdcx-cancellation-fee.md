# Istanbul rules start at `TIPXDCXCancellationFee`, not at the Istanbul or Constantinople row

**Rule.** The Istanbul instruction table (it includes SHL, SHR, SAR, CREATE2, EXTCODEHASH, CHAINID, SELFBALANCE, SLOAD at 800 gas and EIP-2200 SSTORE) is used from the first block at which `IsIstanbul` holds. On mainnet and apothem that block is `TIPXDCXCancellationFee`, tens of millions of blocks before the `istanbulBlock` and `constantinopleBlock` rows. Before it the Byzantium table applies: SHR is invalid and SLOAD costs 200.

**Source** (`6fc8e8c2088b7f18264f5663036669d04fa7becf`)
- `params/config.go:3027` `IsIstanbul`: `IsTIPXDCXCancellationFee || isBlockForked(IstanbulBlock)`.
- `core/vm/evm.go:213-214` the table is chosen by `chainRules.IsIstanbul`.
- `core/vm/jump_table.go:225-234` `newIstanbulInstructionSet` builds on Constantinople and adds 1344, 1884, 2200.

**Networks.** 50 and 51 (value from `generated/forks.json`, `effective.istanbul`). 5551 and 551 from block 0.

**Rust.** `ChainSpec::is_istanbul` feeds `xone_evm::spec::rules`, which returns `SpecId::ISTANBUL` from that block (`PETERSBURG` or `BYZANTIUM` before it). Regression cases `shr-35.4M` and `sload-58.8M` replay real apothem transactions that fail if the switch reads the row instead.

**Legacy.** xOneGo gated Istanbul on the row and executed about 38 million apothem blocks with the wrong table (issue 26).
