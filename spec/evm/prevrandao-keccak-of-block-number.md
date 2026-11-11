# PREVRANDAO is `keccak256(block number)`; before London opcode 0x44 is DIFFICULTY

**Rule.** XDC has no randomness beacon and no mix digest. geth-XDC sets the random value of every block to the Keccak-256 of the block number as a minimal big-endian integer (block 0 hashes the empty string). From `londonBlock` opcode 0x44 returns it; before, it returns the header difficulty.

**Source**
- `core/evm.go:68-75` `random = Keccak256Hash(header.Number.Bytes())` when `IsXDC`.
- `core/vm/jump_table.go:103-116` the Merge marker installs `opRandom` at 0x44.
- `params/config.go:4102` the Merge predicate is `IsLondon` on XDC.

**Networks.** All.

**Rust.** `xone_evm::spec::prevrandao` and `XdcRules::prevrandao_is_number_hash`; `block_env` fills `BlockEnv::prevrandao` from London on. Regression cases `difficulty-56.4M` (before London) and `prevrandao-68.7M`.
