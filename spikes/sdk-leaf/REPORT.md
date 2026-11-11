# Spike XA-03 / S3: can the reth SDK replace the account-leaf encoder and the snap `AccountRange` body?

Issue XDCIndia/xOne#8. Plan item: R4 change C4. Evidence from XA-02 (`spikes/wide-balance/REPORT.md`) is reused,
not repeated. Source read: reth `v2.8.1` (`d763342fa185e4e90ee79dc65f6a5706168b0bcb`, the last revm tag), checked out at
`/mnt/data/tmp/xa03/reth-pins/v2.8.1` (paths below are relative to it); `reth-primitives-traits 0.8.2` and `alloy-trie 0.9.9`
from the crates.io registry (the versions reth pins, `Cargo.toml:402`); and the XDC fork `reth-apothem-spec`
(HEAD `0995c2519745`, reth 2.9.0) for what reth-xdc had to do.

## Verdict

**No.** The pinned reth SDK cannot carry an XDC wide balance in an account, cannot take a different leaf encoder for
the state trie, and cannot take a different snap `AccountRange` body, without changing reth source. The generics of
`NodePrimitives` do not reach the account type. The one place where reth has a leaf-encoder trait (`LeafValueEncoder`)
is bound to the same `Account` type and covers one of five encode sites. reth-xdc is a fork for exactly this reason.
Option B of R4 stays closed for state. Record this in ADR-002 so the question is not asked again.

Proof by test, not only by reading: `spikes/sdk-leaf/probe` feeds the geth-XDC vectors of XA-02 to stock reth and
alloy-trie types. Every narrow value decodes and round-trips; every wide value is rejected by `TrieAccount` and by
the snap `AccountData`.

```
$ cargo +nightly test --manifest-path spikes/sdk-leaf/probe/Cargo.toml -- --nocapture
           0 wide=false TrieAccount: decoded        snap AccountData: decoded
           1 wide=false TrieAccount: decoded        snap AccountData: decoded
     2^256-1 wide=false TrieAccount: decoded        snap AccountData: decoded
       2^256 wide=true  TrieAccount: REJECTED       snap AccountData: REJECTED
     2^256+1 wide=true  TrieAccount: REJECTED       snap AccountData: REJECTED
     2^264-1 wide=true  TrieAccount: REJECTED       snap AccountData: REJECTED
       2^264 wide=true  TrieAccount: REJECTED       snap AccountData: REJECTED
apothem_max_observed wide=true  TrieAccount: REJECTED       snap AccountData: REJECTED
test stock_types_reject_wide_balances ... ok
```

The probe is outside the workspace (it needs rustc 1.95, the workspace pins 1.94.0; see `spikes/net/REPORT.md`). It runs on
`+nightly` (1.96.0-nightly). It reads `spikes/wide-balance/vectors/encoding.json`, which the geth-XDC oracle prints.

## Where `Account { balance: U256 }` is hard-coded

| Layer | Type or function | Location | Replaceable? |
| --- | --- | --- | --- |
| Account type | `pub struct Account { nonce: u64, balance: U256, bytecode_hash, extension }` | `reth-primitives-traits 0.8.2 src/account.rs:31-45` | No. A concrete struct, not a generic. |
| Leaf conversion | `Account::into_trie_account(self, storage_root) -> TrieAccount` and `From<TrieAccount> for Account` | `account.rs:248-262`, `:313-322` | No. |
| Trie leaf type | `pub struct TrieAccount { nonce, balance: U256, storage_root, code_hash, extension }`; the extension is "encoded as a fifth RLP string only when nonempty" | `alloy-trie 0.9.9 src/account.rs:13-30` | No. geth-XDC puts the wide value in the balance item (XA-02, "Encoder replacement"). A fifth item is a different leaf and a different root. |
| State root | `account.into_trie_account(storage_root)` then `.encode(..)` then `hash_builder.add_leaf(..)` | `crates/trie/trie/src/trie.rs:494-498` | No. Inline in `StateRoot`. |
| Witness | `account.into_trie_account(storage_root).encode(&mut rlp)` | `crates/trie/trie/src/witness.rs:234` | No. |
| Proofs | `account.into_trie_account(storage_multiproof.root)` then `.encode(..)` | `crates/trie/trie/src/proof/mod.rs:167-168` | No. |
| Proof verification | decodes the proof leaf with `TrieAccount::decode` and recomputes `into_trie_account(storage_root)` | `crates/trie/common/src/proofs.rs:252`, `:377`, `:1003-1006` | No. |
| Proof v2 | `account.into_trie_account(root).encode(buf)` inside `AccountDeferredValueEncoder::encode` | `crates/trie/trie/src/proof_v2/multiproof_calculator.rs:277` | See "The one hook" below. |
| Sparse trie | takes already encoded leaf bytes (`LeafUpdate::Changed(Vec<u8>)`, `crates/trie/sparse/src/traits.rs:45`), so it is not itself bound to `TrieAccount`; its callers are (witness row) | - | The caller encodes, and every caller is a row above. |
| Cursor over hashed accounts | `type AccountCursor<'a>: HashedCursor<Value = Account>`; storage `Value = U256` | `crates/trie/trie/src/hashed_cursor/mod.rs:32-37` | Value type fixed by the trait. |
| Provider | `fn basic_account(&self, address) -> ProviderResult<Option<Account>>` | `crates/storage/storage-api/src/account.rs:14-19` | No. |
| Database table | `table PlainAccountState { Key = Address; Value = Account }` | `crates/storage/db-api/src/tables/mod.rs:403-406` | No. The `Compact` codec of `Account` is `account.rs:164` in `reth-primitives-traits`. |
| Node generics | `trait NodePrimitives { type Block; type BlockHeader; type BlockBody; type SignedTx; type Receipt }`. **No account type.** | `reth-primitives-traits 0.8.2 src/node.rs:9-25` | The SDK generics let a node choose its block, header, body, transaction and receipt. They do not let it choose the account. |
| Snap body | `AccountData { hash: B256, body: SlimAccountBody }`, `struct SlimAccountBody(TrieAccount)`, encoder `SlimAccountBodyRef { balance: U256, .. }` | `crates/net/eth-wire-types/src/snap.rs:110-115`, `:518`, `:582-591` | No. The message type is in a lower crate with a typed U256 field; the decoder rejects a 33-byte balance (probe above). |
| Snap version | `enum SnapVersion { V2 = 2 }` only; the module "implements the snap/2 (EIP-8189) message definitions" | `crates/net/eth-wire-types/src/snap.rs:6`, `:19-23` | There is no snap/1 in reth. geth-XDC at the pin serves snap/1 (the local node's Hello lists `snap/1`; `eth/backend.go:868-876`). |
| EVM | `revm_state::AccountInfo::balance: U256`; XA-02 shows the journal wrapper is the only no-fork route | `spikes/wide-balance/REPORT.md` "How revm expresses the rows" | Not by SDK components. reth `v2.9.0` and later replaced revm by `evm2`, which XA-02 did not test. |

## The one hook, and why it does not help

`crates/trie/trie/src/proof_v2/value.rs:26-39` defines `trait LeafValueEncoder { type Value; type DeferredEncoder; fn deferred_encoder(..) }`
and `ProofCalculator<TC, HC, VE: LeafValueEncoder>` is generic over it (`proof_v2/calculator.rs:42`). That is a real
extension point, but:

1. It exists for the v2 proof calculator only. `StateRoot`, the witness, the v1 proofs and the proof verification
   (rows above) do not use it. That is one of five encode sites.
2. `Value` must equal `HC::Value` (`calculator.rs:124`), and the account cursor's `Value` is `Account`
   (`hashed_cursor/mod.rs:32`), which carries `U256`.
3. The only account encoder, `AccountValueEncoder` (`multiproof_calculator.rs:227-251`), is private and calls
   `into_trie_account` (`:277`).

## The `account-ext` feature does not help either

reth `v2.8.1` and `alloy-trie 0.9.9` have a cargo feature `account-ext` (`reth-primitives-traits` `Cargo.toml:30-34`;
`crates/ethereum/reth/Cargo.toml:56-65`). It adds `extension` to `Account` and `TrieAccount`. Three limits:

* It is a build-wide feature, not a generic. The account type changes for the whole binary, and `Account` loses `Copy`
  (`account.rs:29`).
* It encodes the extension as a fifth RLP string (`alloy-trie account.rs:23`). geth-XDC encodes the wide value in the
  balance item, with a 33-byte payload (XA-02 table: `2^256` gives `a1 01 00..00`). A stock leaf with the value in a
  fifth item gives a different state root.
* The balance field stays `U256`, so the SDK still cannot present BALANCE and CanTransfer with the wrapped value and
  the state with the true value unless the node adds a side record.

## What reth-xdc did, as an independent confirmation

The XDC fork of reth 2.9.0 vendors `alloy-trie` (`reth-apothem-spec/Cargo.toml:158`, override at `:707`) and adds a
function-pointer registry in `crates/trie/common/src/xdc_overflow.rs:1-26`, plus a process-global JSON-backed store in
`crates/xdc/primitives/src/overflow_store.rs:1-45`. It patched the encode sites of the trie crate to call that
registry (`crates/trie/trie/src/trie.rs:500`, `witness.rs:235`, `proof/mod.rs:167`,
`proof_v2/multiproof_calculator.rs:281`). The comment at `xdc_overflow.rs:19-26` and `overflow_store.rs:12-17` gives the
reason: threading the value through "~40+" signatures. The state is outside the journal, the unwind and the commit
(plan finding V1).

## What this means for the options

* (a) reth SDK without a fork: fails on state. A node on the SDK would carry the wide value in a side record, as
  reth-xdc does.
* (c) a maintained adapter or fork of reth: possible, and it is what reth-xdc already is. The cost is the rebase on
  every reth release (see ADR-002, "Upgrade cost").
* (b) revm, alloy and a few reth crates as libraries: not affected. `alloy_trie::HashBuilder::add_leaf(key, &[u8])`
  takes the encoded leaf (XA-02, "Encoder replacement"), so the leaf encoder, the snap body and the account type are
  owned by `xone-state` and `xone-net`. The crates that decide this (`reth-trie`, `reth-provider`, `reth-eth-wire-types`
  snap module) are not dependencies.

## Not covered

* `evm2` (reth `v2.9.0` and later) was not examined for a journal hook. The XA-02 wrapper was built on revm 43.0.3 only.
* The mdbx table codec for a custom value type was not tried; the table macro names `Account` directly
  (`tables/mod.rs:403-406`), so the answer is expected to be the same.
* The v2.10.1 sources were not re-read line by line. The `Account`, `TrieAccount` and snap sources are the same
  crates.io versions (`reth-primitives-traits 0.8.2`); the reth-side files of this table were read at `v2.8.1` only.
