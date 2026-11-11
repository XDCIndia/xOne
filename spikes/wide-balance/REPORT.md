# Spike XA-02: wide balances on revm 43.0.3

Issue XDCIndia/xOne#7. Oracle: geth-XDC `6fc8e8c2088b7f18264f5663036669d04fa7becf` (`oracle/PIN`).
revm `=43.0.3` (tag `v120`, `6b2e655b`), with and without `account-ext`. Throwaway code; the matrix,
the vectors and this text are what persists.

Verdict: no revm fork is needed. Both adapters run the whole matrix without touching a revm source file.
Option B (journal wrapper with a side map) is green on every row. Option A (`account-ext` carry) is green on
every row but one, and that row cannot be fixed outside revm. Recommendation: B (see "Adapter recommendation").

Contents: [Legal range](#legal-range) - [Matrix](#matrix) - [Encoder replacement](#encoder-replacement) -
[Adapter recommendation](#adapter-recommendation) - [Legacy adjudication](#legacy-adjudication) -
[Findings outside the matrix](#findings-outside-the-matrix) - [Not finished](#not-finished) - [Reproduce](#reproduce)

## Legal range

### Wide accounts found

Recorded heads. Apothem (chain 51): block 85,227,570 (`0x5147832`), hash
`0xf125cd424158d1a9ac302efe5103b7cf7ae8761e3c4fce9197ada1a4fd48d572`. net2040 (chain 2040): block
730,247 (`0xb2487`), hash `0xd2787cfe10894947b94bf06324c29b97d38cb06c9a4c099f2af86e59a276b9f8`.
Mainnet (chain 50): not queried, no mainnet archive was in scope; see the supply argument below.

Proof method for every row: `eth_getProof` at the head block, the `accountProof` nodes hashed back to the
block's `stateRoot` (`tools/archive.py`, `verify`), and the balance read from the leaf RLP. The RPC `balance`
field is the wrapped value and is not used as the true balance.

| chain | address | true balance | wrapped (what BALANCE sees) | nonce | wide since |
| --- | --- | --- | --- | --- | --- |
| Apothem | `0xb6c1f9da410d0739efbe934b4c30dfbee2755233` | `0x2efb09d4c4695f7edc115424b87863b23abd239f2f7a948d8f56530749b43c601` (2.9363 x 2^256) | `0xefb09d4c4695f7edc115424b87863b23abd239f2f7a948d8f56530749b43c601` | 16 | block 30,102,909 |
| Apothem | `0x4398241671b3dd484fe3213a4fb7511f30e7d7c0` | `0x1000000000000000000000000000000000000000000000035c1e5ed14fd6d5fff` (2^256 + 991,649,269,999,999,999,999) | `0x35c1e5ed14fd6d5fff` | 372 | block 46,466,977 |
| net2040 | none | | | | |

Both Apothem accounts are EOAs. net2040: the 10 genesis accounts were read at the head (9 exist, `0x89` is absent); none is wide (the
largest is `0xbcc21003eb1b02f05111168e254e200668333067`, 9.0 x 10^74, below 2^250).

Wide earlier in history, narrow at the head (verified leaves; first and last wide block found by bisection on
verified leaves, assuming one transition between samples):

| address | wide from | last wide block | how it ended |
| --- | --- | --- | --- |
| `0xa65010026b83368ca05df6e8b467985d6de3eac5` | 1,800 | 57,200,398 | `propose` call with 10^25 wei at 57,200,399 |
| `0x38b20a2594939531373efcd2e6aa54d03fc534be` | 2,187 | 1,755,833 | `propose` call with 10^25 wei at 1,755,834 |

Heavy accounts that are narrow at the head: the other seven Apothem genesis accounts funded with 2^256-1,
`0x6c7ff2b9a79ee2b9113966f67a895c09dca3c759` (0), and the contract
`0x2a5c77b016df1b3b0ae4e79a68f8adf64ee741ba` (about 2^245).

### Maximum observed

`0x2ffffffffffffffffffffffffffffffffffffffe6c1949996b3605008e391759c` (3 x 2^256 minus about 1.5 x 10^20,
258 bits, 33 payload bytes). It is the balance of `0xb6c1...5233` from block 31,229,106 to 34,567,914 (it was
sampled at 13 points in that span and is constant after 31,229,106; the maximum is exact only if the balance
never left and returned between samples). It is vector `apothem_max_observed`.

### Method and what it does not prove

Archive RPC cannot enumerate state (`debug_accountRange` is outside the allowed calls). A balance above 2^256-1
needs an account that already holds about 2^256, and a flow below 2^199 cannot create such an account, so
`tools/explore.py` follows every top-level transfer of at least 2^199 wei out of the nine Apothem genesis
accounts funded with 2^256-1 and reads the verified true balance of every account reached. Result: three
genesis accounts (`0x38b2`, `0x4e37`, `0x42e6`) sent about 3 x 2^256 to `0xb6c1`; `0xb6c1` sent it on in
three transfers to `0x6c7f`, which sent a part to the contract `0x2a5c`. No drop was left unexplained.
Not covered: a transfer of that size made from inside a contract, and a balance that left and came back
between two samples (2,000,000-block grid). The list is therefore complete under that method, not proved
complete by enumeration.

Supply argument, which does cover mainnet and net2040. Wide needs a balance of at least 2^256 and balances
cannot exceed total supply. Genesis supply: Apothem 9.0 x 2^256 (nine accounts of 2^256-1), mainnet 2^95,
net2040 2^250. Block rewards add far less than 2^100 over any horizon. So mainnet and net2040 cannot have a
wide account, and no Apothem account can exceed 9.0 x 2^256 + 2^100, below 2^260.

### Bound and how it is enforced

Legal range: `0 <= balance <= 2^264 - 1` (at most 33 payload bytes, 34 bytes with the RLP prefix). It leaves
at least a factor of 16 over the whole Apothem supply (below 2^260) and 6 bits over the observed maximum
(258 bits).
geth-XDC has no bound; the bound is an xOne rule and stays above every reachable value.

Enforcement, all typed errors, never a wrap of the high part:

* `WideBalance::checked_add` returns `WideBalanceError::AboveBound` when the sum exceeds the bound, and
  `checked_sub` returns `Underflow` below zero (`crates/xone-primitives/src/wide_balance.rs`).
* `WideBalance::check_legal` is called by the state layer on every decoded leaf. The RLP codec itself accepts
  any length, as geth does (`state_account.go:138-152`), so a leaf above the bound reaches `check_legal` and
  fails there instead of being truncated. Vector `2^264` (35-byte item in geth) covers this.
* In the EVM adapter an above-bound credit makes `transfer_loaded` return `OverflowPayment`. Spike gap: stock
  revm handlers ignore the `bool` of `incr_balance` in `reimburse_caller` and `reward_beneficiary`
  (`revm-handler-43.0.3 post_execution.rs:88,124`), so a production `xone-evm` must override both to raise
  the error. The gap is unreachable under the supply bound.

`WideBalance` itself: `crates/xone-primitives/src/wide_balance.rs` (BigUint from `num-bigint`, no `unsafe`),
`low()` is the value mod 2^256, `is_wide()`, `checked_add`, `checked_sub`, RLP `Encodable`/`Decodable`.
Tests (`crates/xone-primitives/tests/wide_balance.rs`) read `tests/vectors/wide_balance.json`, printed by the
Go oracle: RLP bytes, `low` and `is_wide` equal Go for 0, 1, 2^256-1, 2^256, 2^256+1, 2^264-1, 2^264 and the
Apothem maximum; a property test checks `decode(encode(x)) == x` and `low` for random 0..40-byte values;
a saturating-`low` mutant is detected; non-canonical RLP is rejected.

## Matrix

Every row is a scenario in `oracle/matrix.go`. The Go program runs it on the pinned `core/state` (StateDB,
journal, snapshots) and `core/vm` + `core.ApplyMessage` (buyGas, CanTransfer, refund, XDC fee path) under
`XDCApothemChainConfig` (constants from `CopyXDCConstants(51)`) at block 40,000,000 (`legacy`: pre-Berlin, pre-Cancun, SELFDESTRUCT deletes) and at
block 72,000,000 (`cancun`: EIP-6780). It prints balances, BigBalance, storage, tx outcome, the raw account
RLP read from the committed trie, and the state root. `vectors/matrix.json` is that output, unedited; 40
scenario instances. The Rust test (`tests/matrix.rs`) loads the same pre-state and operations, runs them on
revm 43.0.3 through each adapter, and compares every printed field, including the state root computed with
the spike's own encoder (not `alloy_trie::TrieAccount`). Gas is not compared, only tx success, balances,
storage and roots.

"=" means equal to geth on every field. Results: `cargo test -p wide-balance-spike` (A and B) and
`cargo test -p wide-balance-spike --no-default-features` (B on revm without `account-ext`).

| Matrix row | Vector ids | geth-XDC result | Option A | Option B | Fork? |
| --- | --- | --- | --- | --- | --- |
| credit to wide | `credit_to_wide` | 2^256-100 + 300 -> 2^256+200, BigBalance set, low 200 | = | = | no |
| debit crossing back below 2^256 | `debit_crossing_back` | 2^256+1000 - 1500 -> 2^256-500, BigBalance cleared; 2^256+1000 - 100 stays wide | = | = | no |
| BALANCE, SELFBALANCE | `balance_selfbalance` (both rules) | all three slots hold the wrapped low part `0x35c1e5ed14fd6d5fff`, for a wide EOA, for BALANCE(this) and SELFBALANCE of a wide contract | = | = | no |
| CanTransfer | `can_transfer_top`, `call_value_wide_contract` | value 5x10^21 above the wrapped balance 9.9x10^20 succeeds (top-level tx and CALL from a wide contract); sender ends narrow at 2^256-4.0x10^21 | = | = | no |
| buyGas / affordability | `buy_gas_wide_sender` | gas x price 1.05x10^22 above the wrapped balance succeeds; sender ends narrow; coinbase gets 1.05x10^22 | = | = | no |
| fee refund | `fee_refund_crosses_up` | 2^256+50000 pays 100000 gas, buyGas drops it to 2^256-50000, refund 79000 crosses up: 2^256+29000, wide, coinbase 21000 | = | = | no |
| block reward | `block_reward`, `coinbase_wide_fee` | credit of 5000 XDC to a wide account stays wide; fee credited to a wide coinbase stays wide | = | = | no |
| CREATE / CREATE2 endowment | `create_endowment`, `create_tx_wide_eoa` | wide contract CREATE and CREATE2 with 5x10^21 (above low) succeed; creation tx from a wide EOA succeeds | = | = | no (needs the `JournaledAccountTr::balance` override, see below) |
| CALL value | `call_value_wide_contract` | see CanTransfer | = | = | no |
| SELFDESTRUCT of a wide contract | `selfdestruct_wide`, `selfdestruct_to_wide`, `selfdestruct_to_self` | beneficiary gets only the wrapped part. legacy: contract deleted, the 2^256 part is burned. cancun (not a new contract): contract stays with true balance 2^256 and low 0. To a wide beneficiary: credit added to the true value. To self: legacy deleted and burned, cancun untouched | = | = | no |
| nested revert at depth >= 2 | `nested_revert_depth2` | P -> Q -> R -> X: R credits X across 2^256 then reverts, X back to narrow; Q's later credit makes X 2^256+50 | = | = | no |
| out-of-gas revert | `oog_child_revert`, `oog_top_wide_sender` | child out of gas: wide contract's transfer undone, still wide; top-level out of gas from a wide sender: value returned, still wide | = | = | no |
| journal revert after a wide transition | `journal_revert_inner`, `journal_revert_all`, `journal_revert_wide_source`, `revert_after_wide_debit` | narrow->wide->narrow->wide chain reverted to the right snapshot each time; wide debit inside a reverted frame leaves the account wide | = | = | no |
| SetBalance clearing | `set_balance_clears` | 2^256+1000 -> SetBalance(12345): BigBalance cleared | = | = | no |
| SetBalance clearing (EIP-161 side effect) | `wide_low_zero_empty` | account with true value 2^256, low 0, nonce 0, no code is "empty" to geth (`stateObject.empty()` tests the wrapped balance, `state_object.go:92-94`), so finalisation deletes it and burns the 2^256 | **differs** | = | no for B; A cannot be fixed outside revm |
| precompile calls | `precompile_value` | wide contract CALLs identity (0x04) with 5x10^21: success, precompile account holds the value | = | = | no |
| 0x89 / 0x90 special txs | deferred to #28 | needs special-tx block processing (no fee, no touch, `types.IsSpecialTx`) that does not exist in xOne yet | - | - | - |
| TRC21 fee path | deferred to #28 | needs TRC21 capacity and issuer-state lookups; the oracle uses the coinbase as fee owner (`core.ApplyMessage` owner argument), which equals revm's coinbase credit | - | - | - |

Divergence, option A, `wide_low_zero_empty.legacy`. `AccountInfo::is_empty` in revm-state 43.0.1
(`account_info.rs:316-321`) also requires an empty extension, so an account whose wrapped balance is zero but
whose extension holds the wide value is not empty to revm. geth deletes it. The test lists it in
`KNOWN_DIVERGENT_A` and fails if the set changes in either direction. The same predicate drives
EXTCODEHASH (`revm-interpreter host.rs:78`) and the new-account gas surcharge (`revm-handler execution.rs:64`),
so a wide account with low 0, nonce 0 and no code would also differ there for the rest of a transaction. Such an
account cannot persist in geth state (it is deleted at the first finalisation), so the exposure is transient.

Mutation checks (each detected; both adapters unless noted): a saturating `low()` (all wide rows fail);
`JournaledAccountTr::balance` reporting the wrapped value (the 4 create rows fail with OutOfFunds); a transfer
that ignores the wide value (rows differ); SELFDESTRUCT without the carry fix-up (rows differ); a checkpoint
revert that does not unwind the carried value (option B only fails: option A reverts through revm's own
`ExtensionChange` entries).

### How revm expresses the rows

Neither option works with stock revm, and neither is a fork. Stock revm hard-codes U256 in four places:
`JournalInner::transfer_loaded` (`revm-context-43.0.3 journal/inner.rs:459-505`, `checked_sub`),
`create_account_checkpoint` (`inner.rs:538-600`), `selfdestruct` (`inner.rs:655-735`), and the affordability
tests (`EthFrame::make_create_frame`, `revm-handler-43.0.3 frame.rs:302`, and `calculate_caller_fee`,
`pre_execution.rs:181`). Stock `incr_balance` also returns `false` on overflow
(`revm-context-interface-43.0.2 journaled_state/account.rs:382-388`). The adapter uses only extension points:

* `WideJournal<DB>` implements `JournalTr` and wraps `Journal<DB>` (`src/journal.rs`). It overrides
  `transfer`, `transfer_loaded`, `selfdestruct`, `create_account_checkpoint`, `balance_incr`, `checkpoint*`;
  the rest delegates. It is plugged in with `Context::new(db, spec)` (the `JOURNAL` type parameter).
* `WideAcct` implements `JournaledAccountTr` and is the `JournaledAccount` associated type. `balance()`
  returns `U256::MAX` for a wide account, which is exactly geth's `CanTransfer` (true for any wide account)
  as revm's `make_create_frame` sees it; `incr_balance`, `decr_balance`, `set_balance` apply the geth rules.
  This is what makes CREATE/CREATE2 work with no `EvmTr` or frame replacement.
* `WideHandler` implements `Handler` and overrides only `validate_against_state_and_deduct_caller` (geth
  `buyGas`: skip the wrapped pre-check for a wide sender, subtract gas cost from the true balance).
  `reimburse_caller` and `reward_beneficiary` need no override: they call `incr_balance` on the wrapper type.

Size: `journal.rs` 612 lines, of which about 390 are the `JournalTr` (38 methods) and `JournaledAccountTr`
(20 methods) impls, mostly delegation; `handler.rs` 73 lines.

## Encoder replacement

Question: can the account-leaf encoder, and the snap `AccountRange` body, be replaced without forking revm or
alloy-trie? Answer: yes, for both. Evidence is test-backed: `tests/encoding.rs` produces the account RLP,
the slim RLP and a one-account trie root equal to geth for all eight boundary values, using an encoder in
`src/harness.rs` that touches neither `TrieAccount` nor any revm type; `tests/matrix.rs` does the same for the
state root of every matrix row.

What makes it possible, by type and function:

* `alloy_trie::HashBuilder::add_leaf(&mut self, key: Nibbles, value: &[u8])`
  (alloy-trie 0.9.9 `src/hash_builder/mod.rs:169`) takes the leaf value already encoded. The encoder is the
  caller's, so a wide balance goes through unchanged.
* What does not work: `alloy_trie::TrieAccount` (`src/account.rs:13-27`) has `balance: U256` and an optional
  fifth RLP string `extension`, which cannot carry the balance (it is item 5, not item 2). The convenience
  functions `state_root`, `state_root_unsorted`, `state_root_unhashed`, `state_root_ref_unhashed`
  (`src/root.rs:120-162`) are bounded by `A: Into<TrieAccount>`. They are fine for genesis (all genesis
  balances fit; `xone-chainspec/src/genesis.rs:145-152` uses them) and must not be used for any post-genesis
  root.
* revm has no account encoder: `revm_state::AccountInfo` does not implement `Encodable`, and state roots are
  computed outside revm. So nothing in revm blocks a replacement.
* The snap `AccountRange` body is an xOne-owned protocol message, not an alloy-trie or revm type. The slim
  item is `[nonce, balance, root-or-empty-string, codehash-or-empty-string]` (geth `types.SlimAccountRLP`,
  `state_account.go:91-118`); `encode_slim` equals it for all eight vectors, and it decodes through the same
  unbounded `WideBalance` decoder (`FullAccount`, `state_account.go:131-152`).

Encoding table, geth equal to xOne on every row (full vectors in `vectors/encoding.json`; account RLP uses
nonce 7, empty storage root, empty code hash):

| value | balance item (RLP) | item bytes | account RLP bytes | slim RLP bytes | one-account trie root |
| --- | --- | --- | --- | --- | --- |
| 0 | `80` | 1 | 70 | 5 | `0x28641138be585108..` |
| 1 | `01` | 1 | 70 | 5 | `0xae9b87fbd4f02a65..` |
| 2^256-1 | `a0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff` | 33 | 102 | 37 | `0x714b00f6a9111752..` |
| 2^256 | `a1010000000000000000000000000000000000000000000000000000000000000000` | 34 | 103 | 38 | `0x3af4bd306ea01ad9..` |
| 2^256+1 | `a1010000000000000000000000000000000000000000000000000000000000000001` | 34 | 103 | 38 | `0x8e7cac13c076378a..` |
| 2^264-1 | `a1ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff` | 34 | 103 | 38 | `0x6c587b948d7a8ef3..` |
| 2^264 | `a201000000000000000000000000000000000000000000000000000000000000000000` | 35 | 104 | 39 | `0x279ec8c7df6b6ce6..` |
| Apothem maximum | `a102ffffffffffffffffffffffffffffffffffffffe6c1949996b3605008e391759c` | 34 | 103 | 38 | `0x3c4a83191b545d2b..` |

"Always 33 bytes" is the payload length only for values from 2^256 to 2^264-1; 2^264 needs 34.

## Adapter recommendation

Recommendation: option B, the journal wrapper with a side map, no revm fork. Do not trigger R5.

| | Option A: `account-ext` carry | Option B: wrapper with side map | Fork |
| --- | --- | --- | --- |
| Matrix | 1 row red (`wide_low_zero_empty`, revm's `AccountInfo::is_empty`) | all rows green | not needed |
| Where the wide value lives | `AccountInfo::extension` (bytes of the true value) | `SideMap` inside the journal wrapper | - |
| Rollback | revm's own `ExtensionChange` entries (`revm-context-interface-43.0.2 journaled_state/entry.rs:126,351-356`) | own undo log plus a mark stack kept in step with every revm checkpoint, including the one `create_account_checkpoint` opens itself | - |
| Needs a wrapper anyway | yes (`JournalTr`, `JournaledAccountTr`, handler override) | yes (same) | - |
| Copy loss / layout | `AccountInfo` 88 -> 96 bytes; `copy_without_code` stops being `const fn` and clones a `ThinArc`; postcard and bincode of accounts unsupported (`revm-state account_extension.rs` doc) | none | - |
| Perf, narrow transfer | 42-54 ns vs 25-31 ns stock (measured, noisy shared host) | 42-54 ns | - |
| Upgrade | wrapper surface (`JournalTr` 38 methods, `JournaledAccountTr` 20) plus the young opt-in feature | wrapper surface only | rebase on every revm bump |
| Persistence | free: extension flows through `Database::basic`, `EvmState`, commit | needs a lazy-load hook (`DB: WideSource`), a flush at commit, and `forget` for deleted accounts (a stale entry would resurrect a wide value) | - |

Notes on the costs:

* Perf was measured with `cargo test --release -p wide-balance-spike --test perf -- --ignored --nocapture`
  (2,000,000 narrow transfers). The numbers above include the narrow fast path (neither side wide, nothing
  overflows: call stock `transfer_loaded`). Without it the wrapper took about 450 ns per transfer because
  every operation built a `BigUint`; the fast path is therefore mandatory in `xone-evm`. A and B are equal
  within noise. The cache cost of A's 8 extra bytes per account was not measured.
* Why B over A. B is the only adapter with a fully green matrix. A's red row comes from a predicate inside
  `revm-state` that the extension changes for every account; the only repair is a revm change. A's one real
  advantage (free rollback and persistence) is an engineering cost in B, not a correctness gap: the checkpoint-revert
  mutation above shows where B can go wrong, and the matrix catches it.
* A is the fallback if B's lifecycle code (undo marks, lazy load, `forget`) proves fragile in `xone-state`.
  Accepting A means accepting that gas and EXTCODEHASH differ from geth for a transient wide account with a
  wrapped balance of zero, nonce 0 and no code.
* R5 (fork) is not triggered: no row needed a change in a revm crate.

### Legacy adjudication

Goal of the issue: for history blocks that touch a wide account with BALANCE or SELFDESTRUCT, geth-XDC's result
equals the canonical receipt and state root.

Result: no block with BALANCE or SELFDESTRUCT touching a wide account was found, so that part of the item is
open. What was searched: all wide Apothem accounts are EOAs (no SELFBALANCE and no SELFDESTRUCT of a wide
contract can occur); all 13 transactions sent by `0xb6c1` while wide (nonce 3 to 16, found by bisection
on `eth_getTransactionCount`) and its balance-change blocks after block 34,567,934 (8 found, 3,000,000-block grid); `0x4398`, which
sent nothing while wide (nonce 372 at block 46,466,977 and at the head) and whose balance is the same at
block 60,000,000 and at the head; the contracts called in cases C, D and F (validator contract `0x88`, router `0x3f11...`) contain no BALANCE, SELFBALANCE or
SELFDESTRUCT opcode. A contract reading a wide account with BALANCE would have to be found by scanning 85
million blocks, and the allowed archive calls (no state enumeration, 5 requests per second) cannot do that.
The question "did legacy v2.6.8 push a value above 2^256 for BALANCE" stays open and needs either a different
data source or an owner decision.

What was adjudicated instead: six historical blocks where a wide account is touched by the same balance
operations (credit across 2^256, debit across 2^256 with value above the wrapped balance, internal CALL
credit, wide sender calling a contract). For each, `tools/adjudicate.py` takes the pre-state from the
archive (`prestateTracer`, true balances from verified leaves), runs the transaction in the pinned geth-XDC
oracle (`oracle replay`) at the block's number and timestamp, and compares (a) every balance and storage
slot of the archive's own diff trace and (b) the wide account's leaf with the leaf of the canonical trie at
that block, verified against the header `stateRoot`. Output: `vectors/adjudication.json`.

| Case | Block | Transaction | Touch | header stateRoot / receiptsRoot | Result |
| --- | --- | --- | --- | --- | --- |
| A | 30,102,909 | `0xee99665f4a60...` | plain transfer credits `0xb6c1` across 2^256 (narrow -> wide) | `0x2646a05dce...` / `0x91e8e52a69...` | oracle post-state = trace; wide true balance and leaf RLP = canonical leaf |
| B | 46,466,977 | `0x31e5798142ee...` | plain transfer credits `0x4398` across 2^256 | `0x92232f9fd7...` / `0x448360f9c3...` | same, leaf RLP equal |
| E | 41,438,509 | `0x8421f26a5220...` | multisig `execTransaction` pays `0xb6c1` (a CALL value inside a contract) | `0x29d66e8f01...` / `0x9cc7a10a10...` | same, leaf RLP equal |
| F | 35,331,522 | `0x07db7ca2b864...` | wide `0xb6c1` calls a router with 111,111 XDC (`swapExactETHForTokens`) | `0x1d631d1b0b...` / `0xf71e1e33ff...` | oracle storage and balances = trace (the pair contract's timestamp slots match once the header timestamp is supplied); leaf RLP equal |
| C | 57,200,399 | `0xc8f988395cb9...` | wide `0xa650` calls the validator contract with 10^25 wei (wrapped balance 8.5x10^23): CanTransfer and buyGas on the wide path, debit across 2^256 | `0x3bb76acd9f...` / `0x49d292f1e2...` | receipt status 1. Not replayable (archive trace fails, see findings). Canonical arithmetic: post = pre - value - own fee + all block fees, exactly, because `0xa650` is also the fee owner of the block |
| D | 1,755,834 | `0x1ecf61d89f40...` | wide `0x38b2` calls the validator contract with 10^25 wei (wrapped balance 9.99999999789999x10^23), debit across 2^256 | `0x33e594739a...` / `0x2c96c5ac99...` | receipt status 1. Canonical arithmetic: post = pre - value - fee, exactly |

The oracle's result equals the canonical chain for every case that could be replayed (A, B, E, F), and the
canonical leaves for C and D follow the true-integer arithmetic. Cases C and D are the history evidence for
CanTransfer, buyGas and debit-below-2^256 with value above the wrapped balance. Because geth-XDC at the pin
produced these canonical roots under the semantics the matrix reproduces (wrapped value on the stack, true
value in the leaf), an xOne that equals the oracle on the matrix is consistent with history for these touches.
It is not evidence about BALANCE or SELFDESTRUCT, see above.

## Findings outside the matrix

1. geth-XDC loses the wide semantics when tracing is on. `core/evm.go:158` (`CanTransfer`) and
   `core/state_transition.go:561` (`buyGas`) find `GetBigBalance` by an interface assertion on the state.
   Tracing wraps the StateDB in `hookedStateDB` (`core/state/statedb_hooked.go:39-45`,
   `eth/tracers/api.go:993`, `core/state_processor.go:145`), which has no `GetBigBalance`, so the assertion
   fails. The archive confirms it: `debug_traceTransaction` of case C fails with
   "insufficient funds for gas * price + value: address 0xa650... have 850139385444201194569443 want
   10000000037500000000000000", and case D fails the same way. Untraced block import is unaffected (the roots
   verify). The oracle runs untraced. xOne tracing must not change execution semantics.
2. Stock revm drops a credit above U256 silently: `incr_balance` returns false and
   `reimburse_caller`/`reward_beneficiary` ignore it (`post_execution.rs:88,124`).
3. A wide value whose wrapped part is zero is deleted by geth at EIP-161 finalisation if nonce is 0 and there is
   no code (row `wide_low_zero_empty`): the 2^256 is burned. It cannot appear in persisted state.
4. `cargo test -p xone-primitives` alone did not build before this change (the `Decodable` impl for `U256`
   needs alloy-primitives' `rlp` feature, which only `alloy-trie` pulled in). The workspace dependency now
   enables `rlp`.

## Not finished

* Mainnet: no archive access in scope; the claim rests on the supply argument.
* Legacy adjudication for BALANCE and SELFDESTRUCT: no instance found (see above).
* 0x89/0x90 special txs and the TRC21 fee path: deferred to #28 (need block-processing machinery).
* Gas is not compared; Prague, Osaka and Amsterdam rules are not run (Apothem at blocks 40M and 72M only).
* The wide-account list is complete under the stated method, not by enumeration.
* `oracle/` runs against a geth-XDC checkout supplied by `GETH_SRC`; a different checkout than `oracle/PIN`
  gives different vectors.

## Reproduce

```sh
# vectors (writes vectors/*.json and crates/xone-primitives/tests/vectors/wide_balance.json)
GETH_SRC=<geth-XDC checkout at oracle/PIN> spikes/wide-balance/oracle/run.sh
cargo test -p xone-primitives
cargo test -p wide-balance-spike                      # options A and B on revm with account-ext
cargo test -p wide-balance-spike --no-default-features  # option B on revm without account-ext
cargo test --release -p wide-balance-spike --test perf -- --ignored --nocapture
# archive evidence (read-only, 5 requests per second over ssh host xdc02)
python3 spikes/wide-balance/tools/explore.py
GETH_SRC=<checkout> python3 spikes/wide-balance/tools/adjudicate.py spikes/wide-balance/tools/adjudicate_cases.json out.json
```
