# The fee market, EIP-2929 and EIP-3860 follow `Eip1559Block`, not London

## Rule

On XDC, London fires well before the fee market. Several London-era rules are moved to `Eip1559Block`:

- `IsEIP1559(n)` is the `Eip1559Block` row alone. When the row is `nil` it is false. (The doc comment above it says it falls back to `IsLondon`; the code does not.)
- EIP-2929 (cold/warm access) is active at `Eip1559Block`, not Berlin.
- EIP-3860 (initcode limit) is active at `Eip1559Block`, not Shanghai.
- The EIP-3529 refund cap (`/5`) applies from `Eip1559Block`, not London.
- Type 1 and type 2 transactions are accepted from `Eip1559Block` (signer choice).
- The EVM jump table switches to the Shanghai set at `Eip1559Block` (before Cancun).
- The genesis base fee is gated on `Eip1559Block` (see [genesis-header](genesis-header.md)).
- `BASEFEE` between London and `Eip1559Block` reads a fixed tier price because XDC headers carry no base fee yet.

Merge and Shanghai rules follow `LondonBlock`; Cancun, Prague, Osaka and Amsterdam follow their own `*Block` rows.

## Source

- `params/config.go:3046-3051` `IsEIP1559`.
- `params/config.go:4075-4078` EIP-2929; `:4102-4105` Merge, Shanghai (London), Cancun (`CancunBlock`), EIP-3860 (`Eip1559Block`); `:4108`, `:4112`, `:4116` Prague, Osaka, Amsterdam by block.
- `core/state_transition.go:1351-1357` refund quotient.
- `core/types/transaction_signing.go:52-57` signer.
- `core/vm/evm.go:185-188` jump table.
- `core/genesis.go:1121-1124` genesis base fee.
- `core/vm/eips.go:228-230` `BASEFEE` with a `nil` header base fee.

## Networks

All four. Devnet (5551) and devnet551 (551) have London at 0 and `Eip1559Block` at 25,000; mainnet and apothem have London well before `Eip1559Block`.

## Rust

- `ChainSpec::is_eip1559`, `is_eip2929`, `is_eip3860`; `genesis::Genesis::to_header` uses `eip1559` for the base fee.
- Tests: `eip1559_is_its_own_row_not_london` (minus one, exact, plus one for 50 and 51; London alone does not enable it; no fallback when the row is `Never`), `base_fee_follows_eip1559_block_not_london`.
- Execution behaviour is not implemented.

## Legacy

geth-XDC cites legacy `core/types/transaction_signing.go:41-55` and `core/state_transition.go:594-603` (v2.9.3-testnet) for the signer and refund rules. Not re-verified here.
