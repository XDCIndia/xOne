# Account leaf: the XDC state account RLP, its slim form, and the secure-trie keys

## Rule

- The state trie leaf is the RLP list `[nonce, balance, root, codeHash]`. Nonce is an RLP unsigned integer. `root` and `codeHash` are 32-byte strings.
- The balance item is the true balance as an RLP big integer of any width: minimal big-endian bytes, zero is the empty string `0x80`. geth writes `BigBalance` when it is set and `Balance` (the value mod 2^256) when it is not. `BigBalance` is set only when the true value is above 2^256-1, so for every value up to 2^256-1 the bytes equal upstream geth.
- Boundary lengths of the balance item: 2^256-1 is a 33-byte item (`a0` + 32), 2^256 and 2^256+1 are 34-byte items (`a1` + 33), 2^264-1 is `a1` + 33 bytes, 2^264 needs `a2` + 34 bytes.
- The decoder reads the balance as a big integer of any length. A leading zero byte is an error (non-canonical integer). A missing balance is zero.
- Slim form (snap): `[nonce, balance, root, codeHash]` where the empty storage root and the empty code hash are written as the empty string. The balance item is the same big integer, so a wide balance goes through in full. The slim decoder gives the empty root and the empty code hash back for an empty string.
- Trie keys: account key is `keccak256(address)`. Storage key is `keccak256(slot)` over the 32-byte slot. A storage value is the RLP of the slot word with leading zeros trimmed; a zero word is a deletion, not a leaf. An account with no non-zero slot has the empty-trie root `56e81f17...b421`.
- An account whose wrapped balance is zero, nonce is zero and code hash is empty is "empty" to geth even when its true balance is 2^256; that is an execution rule (EIP-161 finalisation), not a trie rule. The trie writes whatever leaf it is given.
- After genesis, no root may be built through `alloy_trie::TrieAccount` or `alloy_trie::root::state_root*`: they carry a `U256` balance and cannot hold a wide one. xOne encodes the leaf itself and passes the bytes to `HashBuilder::add_leaf`.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `core/types/gen_account_rlp.go:14-32` `StateAccount.EncodeRLP`: `BigBalance` (`WriteBigInt`) if set, else `Balance` (`WriteUint256`), else the empty string; `Root` and `CodeHash` as byte strings.
- `core/types/gen_account_rlp.go:42-66` `DecodeRLP`: balance decoded as `*big.Int`; `BigBalance` kept only on uint256 overflow.
- `core/types/state_account.go:34-47` the struct and the `BigBalance` field; `:80-86` `SlimAccount`.
- `core/types/state_account.go:89-125` `SlimAccountRLP`, `SlimAccountRLPInto`: `BigBalance` first (`:106-108`), empty root and code hash as `nil` (`:113-122`).
- `core/types/state_account.go:138-169` `FullAccount`: balance as big integer, empty root and code hash restored; `:172-178` `FullAccountRLP`.
- `trie/secure_trie.go:213-224` `UpdateStorage` (key hashed, value `rlp(trimmed bytes)`); `:250-263` `UpdateAccount` (`keccak(address)`, `rlp.EncodeToBytes(acc)`); `:305-320` deletions.
- `core/state/statedb.go:1008-1021` a zero pending value deletes the slot, a non-zero one is written trimmed.
- `core/state/state_object.go:92-94` `empty()` tests the wrapped balance.
- `core/state/reader.go:101-115` the flat reader keeps `BigBalance` (`:112`): a flat read must not narrow it.
- `core/types/hashes.go:26` `EmptyRootHash`.

## Networks

All four (mainnet 50, apothem 51, devnet 5551, devnet551 551). Wide balances exist on Apothem (two accounts, `spikes/wide-balance/REPORT.md`, "Legal range") and are legal up to 2^264-1 in xOne (`xone_primitives::MAX_WIDE_BITS`).

## Rust

- `xone_state::Account` (`src/account.rs`): `rlp()`, `slim_rlp()`, `from_slim_rlp()`, `Encodable`/`Decodable`.
- `xone_state::trie::{root_of_hashed, storage_root, state_root, flat_root}` (`src/trie.rs`).
- Oracle: `crates/xone-state/oracle/` (pin in `oracle/PIN`, run `oracle/run.sh`). Vectors: `tests/vectors/accounts.json` (24 vectors: six balances including 2^256-1, 2^256, 2^256+1, 2^264-1, times empty and non-empty root and code hash; full RLP, slim RLP, geth's slim to full) and `tests/vectors/mutations.json` (50 seeded mutations with the root after each).
- Tests: `account_leaf_and_slim_equal_geth`, `slim_refuses_odd_hash_lengths`, `fifty_mutations_root_equals_geth_at_every_step`, `narrowing_a_wide_balance_changes_the_root` (mutant: a wrapped leaf changes the root), `genesis_alloc_roots_equal_the_chainspec_roots`.

## Legacy

- geth reads `CodeHash` as a free-length byte string and the slim root and code hash the same way. Rust accepts only 32 bytes (full form) or 0 or 32 bytes (slim) and returns `SlimError::BadHashLength` or an RLP error otherwise. No honest encoder writes another length. If the owner wants the geth behaviour, this is the one place to change.
- The incremental trie is not in this slice; `trie` recomputes a root from the whole table (see the `ponytail:` note in `src/trie.rs`).
