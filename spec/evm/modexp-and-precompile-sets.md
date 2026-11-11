# Precompile sets per fork row, MODEXP without EIP-2565 before `Eip1559Block`

**Rule.** The set of precompiles is a function of the rules, not of an Ethereum fork:
- Homestead: 0x01 to 0x04. Byzantium: plus MODEXP (Byzantium price), bn256 add, mul, pairing (Byzantium price).
- Istanbul rules: bn256 at Istanbul price, plus BLAKE2F 0x09.
- Berlin row without `Eip1559Block`: MODEXP stays at the Byzantium price (`PrecompiledContractsXDCBerlin`). From `Eip1559Block`: EIP-2565.
- XDCx precompiles 0x1e, 0x28, 0x29, 0x2a exist from Byzantium until `TIPXDCXMinerDisable`, except in the EIP-1559 Berlin set. They are not in the access-list set (`ActivePrecompiles`), only in the set a call runs.
- Cancun: no 0x0a (a call to it is a call to an empty account). Prague: BLS12-381 at 0x0b to 0x11, and 0x0a is a stub that costs 50,000 gas and always fails.

**Source**
- `core/vm/contracts.go:110-120` `PrecompiledContractsXDCBerlin`; `:240-286` selection; `:293-315` the access-list counterpart.
- `core/vm/contracts_xdc.go:53-58` the KZG stub; `:92-125` Cancun and Prague sets; `:144-206` the XDCx sets, 0x29 and 0x2a at `:144-158`.
- `core/vm/contracts.go:1706-1752` 0x1e and 0x28 (ring signature and bulletproof verification).

**Networks.** All.

**Rust.** `xone_evm::precompiles::row` and `PrecompileRow`; `XdcPrecompiles` is the revm `PrecompileProvider`. The vector test `precompile_sets_equal_geth_at_every_fork_row` compares the executing set, the access-list set and the gas of every precompile for a fixed probe input at row - 1, row and row + 1 on mainnet, apothem and both devnets. Not covered: 0x1e and 0x28 are registered at their address and fail with a fatal error, because the proof systems (`core/vm/privacy`) are not ported.
