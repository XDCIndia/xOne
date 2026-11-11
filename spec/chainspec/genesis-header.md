# Genesis header: always-encoded XDC fields and the base fee rule

## Rule

- The XDC header has three byte fields after `nonce`: `Validators`, `Validator`, `Penalties`. They are always RLP-encoded, even when empty (an empty string, `0x80`). They are not optional trailing fields.
- The optional trailing fields follow: `BaseFee`, `WithdrawalsHash`, `BlobGasUsed`, `ExcessBlobGas`, `ParentBeaconRoot`, `RequestsHash`, then the Amsterdam fields. A `nil` optional field is written as an empty string only when a later one is set.
- The genesis block sets the three XDC fields to empty slices.
- Genesis `BaseFee` is set only if the fee market is active at block 0, and on XDC chains that means `IsEIP1559(0)`, not `IsLondon(0)`. If the genesis gives no base fee, `params.InitialBaseFee` (1,000,000,000) is used. Devnet 5551 has London at 0 and `Eip1559Block` at 25,000, so its genesis has no base fee; a London test would add one and change the hash.
- A zero genesis gas limit becomes `params.GenesisGasLimit`.
- Prague and Amsterdam at the genesis block add header fields (requests hash, slot number, BAL hash). Rust refuses such a genesis with `Error::Unsupported` instead of emitting a wrong hash.

## Source

- `core/types/block.go:86-88` the three fields; `core/types/gen_header_rlp.go:60-63` always encoded, `:64-84` optional tail.
- `core/genesis.go:1086-1104` header construction with the empty XDC fields.
- `core/genesis.go:1105-1107` gas limit; `:1115-1131` base fee rule.
- `core/genesis.go:1161-1170` Prague and Amsterdam fields.

## Networks

All four. The hashes and state roots are in `generated/forks.json` under `genesis`.

## Rust

- `Genesis::to_header`, `Genesis::hash`, `Genesis::state_root`; `xone_primitives::Header` encoding.
- Tests: `genesis_hash_mainnet`, `genesis_hash_apothem`, `genesis_hash_devnet`, `genesis_hash_devnet551` (constants in `params/config.go:40-44`), `generated_table_loads_and_matches_genesis` (Rust hash and state root equal what `ToBlock()` returned in the exporter), `base_fee_follows_eip1559_block_not_london`, `unsupported_genesis_forks_are_refused`.

## Legacy

geth builds the genesis from the `config` section of the embedded genesis JSON, and for devnet551 from `params.XDCDevnet551ChainConfig` (`core/genesis_xdc.go:96-103`). Rust reads the table instead; the hash tests prove both give the same header.
