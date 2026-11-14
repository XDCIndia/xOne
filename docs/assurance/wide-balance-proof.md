# Wide-balance proof: geth-XDC semantics on revm 43.0.3 (XA-02, issue #7)

Status: a proof document assembled from the vectors and tests that are on `main`. It states what is proved, by which file, and what is not. It adds no code and no vector.

Pins. geth-XDC oracle `XDCIndia/go-ethereum` `6fc8e8c2088b7f18264f5663036669d04fa7becf` (`spikes/wide-balance/oracle/PIN`, `conformance/PIN`). revm `=43.0.3` (`crates/xone-evm/Cargo.toml:15`, `Cargo.lock`). xOne base `origin/main` `a99fcf7290fc9a78be30a700badb7504a931be2a`. Spike report: `spikes/wide-balance/REPORT.md`. Spec page: `spec/evm/wide-balance.md`. ADR-002 decision 1 and 2 rest on this document.

## 1. Claim

At revm `=43.0.3`, with no change to a revm, alloy-trie or reth source file, xOne reproduces geth-XDC for balances above 2^256 - 1 ("wide balances") on every path of the matrix in section 4, and reproduces the account RLP, slim RLP and trie leaf for every boundary value in section 3.

The semantics being reproduced (geth-XDC at the pin; from issue #7 and `REPORT.md`):

1. An account holds two values: the wrapped low 256 bits (`StateAccount.Balance`) and the true integer (`BigBalance`, set only when the true value is above 2^256 - 1).
2. `BALANCE`, `SELFBALANCE` and `SELFDESTRUCT` use the wrapped value.
3. `CanTransfer` is true for any wide account; `buyGas` skips the wrapped pre-check.
4. `AddBalance` and `SubBalance` change both values and journal both; `SetBalance` clears `BigBalance`.
5. The account RLP writes the true integer as an unbounded RLP big integer.

## 2. Inputs and their integrity

| Input | Path | SHA-256 | What it is |
| --- | --- | --- | --- |
| Matrix | `spikes/wide-balance/vectors/matrix.json` | `43dee0a83236479ee8b992209d521df5d9a83ca8589fccf35fb25d3a3e579d01` | 40 scenario instances printed by the Go oracle on the pinned `core/state` and `core/vm`: balances, `BigBalance`, storage, tx outcome, raw account RLP, state root |
| Encoding | `spikes/wide-balance/vectors/encoding.json` | `fbeadef56ca5df83a231425ec39314b9abbd24a2a3d74455086daac37db3d2ef` | 8 boundary values: account RLP, slim RLP, one-account trie root |
| Encoding copy | `crates/xone-primitives/tests/vectors/wide_balance.json` | same as above | the byte-identical copy that `xone-primitives` reads |
| Adjudication | `spikes/wide-balance/vectors/adjudication.json` | `e065d7e8592b65fb5281f0fcfe7ad5aaa7797d9cde4450cb1bee514a5418ef6d` | 6 historical Apothem blocks: oracle replay against the archive trace and the canonical leaf |

The vectors are printed by `spikes/wide-balance/oracle/run.sh` and never edited by hand (`REPORT.md`, "Reproduce"). The two encoding files have the same hash, so the primitives test and the spike test read the same oracle output.

## 3. Encoding: equal to geth for eight values

`REPORT.md` section "Encoder replacement" has the byte table. Evidence by test:

| Obligation (issue #7) | Test | What it checks |
| --- | --- | --- |
| Account RLP, trie leaf and snap slim body equal geth at 2^256 - 1, 2^256, 2^256 + 1, 2^264 - 1, 2^264 and the observed maximum (plus 0 and 1) | `spikes/wide-balance/tests/encoding.rs::leaf_slim_and_root_match_go` | account RLP, slim RLP and the one-account root from an encoder that uses neither `TrieAccount` nor a revm type, for all 8 vectors |
| The production type encodes the same bytes | `crates/xone-primitives/tests/wide_balance.rs::matches_go_oracle` | `WideBalance` RLP bytes, `low()` and `is_wide()` equal Go for all 8 vectors |
| Production leaf and slim form | `crates/xone-state/tests/oracle.rs::account_leaf_and_slim_equal_geth` | `Account` full and slim RLP equal the oracle |
| Production root over a sequence containing wide accounts | `crates/xone-state/tests/oracle.rs::fifty_mutations_root_equals_geth_at_every_step`, `::fifty_mutations_unwind_to_every_step_root_and_open_every_block` | state root at each of 50 steps, and after unwinding |
| A leaf that carries only the wrapped part is detected | `crates/xone-state/tests/oracle.rs::narrowing_a_wide_balance_changes_the_root` | the root changes when a wide balance is narrowed to `low()` (the stock `TrieAccount` behaviour) |
| Round trip, bound, non-canonical RLP | `crates/xone-primitives/tests/wide_balance.rs::round_trip_and_low`, `::arithmetic_and_bounds`, `::decode_rejects_non_canonical` | `decode(encode(x)) == x`, `low`, 2^264 - 1 as the legal maximum, typed errors |
| The test would notice a saturating `low()` | `crates/xone-primitives/tests/wide_balance.rs::saturating_low_mutant_is_detected` | mutant check |

The 35-byte item for 2^264 is a vector. It proves the codec accepts what geth writes and that the legal bound (section 5) is an xOne rule, not a codec limit.

## 4. Matrix: equal to geth on every row

40 instances, each at two rule sets: `legacy` (block 40,000,000: pre-Berlin, pre-Cancun) and `cancun` (block 72,000,000), on Apothem parameters. Compared fields: balances, `BigBalance`, storage, tx success, account RLP, state root. Gas is not compared.

| Test | Runs the matrix through | Expected |
| --- | --- | --- |
| `spikes/wide-balance/tests/matrix.rs::option_b_side_map` | the spike's journal wrapper with a side map (option B), revm without `account-ext` | all 40 equal |
| `spikes/wide-balance/tests/matrix.rs::option_a_account_ext` (feature `account-ext`) | revm's `account-ext` carry (option A) | all equal except `wide_low_zero_empty.legacy`, listed in `KNOWN_DIVERGENT_A`; the test fails if that set changes either way |
| `crates/xone-evm/tests/wide_matrix.rs::matrix_equals_geth` | **production** `WideJournal` and `XdcHandler` from `crates/xone-evm` | all 40 equal, no known-divergent list |

Rows covered (the issue's matrix, by vector id): credit to wide (`credit_to_wide`); debit crossing back (`debit_crossing_back`); `BALANCE` and `SELFBALANCE` (`balance_selfbalance`); `CanTransfer` (`can_transfer_top`, `call_value_wide_contract`); `buyGas` (`buy_gas_wide_sender`); fee refund (`fee_refund_crosses_up`); block reward (`block_reward`, `coinbase_wide_fee`); CREATE and CREATE2 endowment (`create_endowment`, `create_tx_wide_eoa`); CALL value; SELFDESTRUCT of a wide contract, to a wide beneficiary and to self (`selfdestruct_wide`, `selfdestruct_to_wide`, `selfdestruct_to_self`, both rule sets); nested revert at depth 2 (`nested_revert_depth2`); out-of-gas revert (`oog_child_revert`, `oog_top_wide_sender`); journal revert after a wide transition (`journal_revert_inner`, `journal_revert_all`, `journal_revert_wide_source`, `revert_after_wide_debit`); `SetBalance` clearing (`set_balance_clears`); EIP-161 emptiness of a wide account with a zero wrapped part (`wide_low_zero_empty`); precompile call with value (`precompile_value`).

Mutation checks recorded in `REPORT.md` (each detected): saturating `low()`; `JournaledAccountTr::balance` returning the wrapped value (the four create rows fail); a transfer that ignores the wide value; `SELFDESTRUCT` without the carry fix-up; a checkpoint revert that does not unwind the carried value (option B only). The production builder has a related mutant in `crates/xone-builder/tests/mutants.rs` ("balance read as the whole wide value").

## 5. Legal range and how it is enforced

Bound: `0 <= balance <= 2^264 - 1` (33 payload bytes, 34 with the RLP prefix). geth-XDC has no bound; this is an xOne rule chosen above every reachable value.

| Fact | Evidence |
| --- | --- |
| Two Apothem accounts are wide at block 85,227,570: `0xb6c1f9da...5233` (true balance 2.9363 x 2^256) and `0x43982416...d7c0` (2^256 + 991,649,269,999,999,999,999) | `REPORT.md` "Wide accounts found"; proofs hashed to the header `stateRoot` by `spikes/wide-balance/tools/archive.py` |
| Maximum observed: 258 bits, `0x2fff...759c`, vector `apothem_max_observed` | `REPORT.md` "Maximum observed" |
| net2040: none wide at the recorded head (block 730,247) | `REPORT.md` |
| Supply argument: Apothem genesis 9.0 x 2^256, mainnet 2^95, net2040 2^250; rewards far below 2^100; so no Apothem account exceeds 9.0 x 2^256 + 2^100, below 2^260 | `REPORT.md` "Method and what it does not prove" |
| A credit above the bound is a typed error, not a wrap | `WideBalance::checked_add` returns `AboveBound` (`crates/xone-primitives/src/wide_balance.rs`); `WideJournal::fault` fails the transaction (`crates/xone-evm/src/journal.rs:91`); `check_legal` on every decoded leaf (`wide_balance.rs:63`) |

The list of wide accounts is complete under the stated search method (follow every transfer of at least 2^199 wei out of the nine Apothem genesis accounts), not by enumeration: archive RPC cannot enumerate state.

## 6. Adapter choice

Recorded in `REPORT.md` "Adapter recommendation" and adopted by ADR-002 decision 1: option B (journal wrapper with a side map). Option A (`account-ext`) is green on all rows but one, and that row (`wide_low_zero_empty.legacy`) cannot be fixed outside revm because revm's `AccountInfo::is_empty` also tests the extension. Zero revm patches. The production crate implements B (`crates/xone-evm/src/journal.rs`, `handler.rs`); the narrow fast path calls stock revm.

## 7. Legacy adjudication

Six historical Apothem blocks that touch a wide account (`vectors/adjudication.json`; `tools/adjudicate.py`). For the four that can be replayed (cases A, B, E, F: blocks 30,102,909, 46,466,977, 41,438,509, 35,331,522) the oracle's post-state equals the archive trace and the wide leaf equals the canonical leaf verified against the header `stateRoot`. For C and D (blocks 57,200,399, 1,755,834: wide sender calls the validator contract with value above its wrapped balance) the canonical receipt status is 1 and the post-balance equals the true-integer arithmetic exactly. These are history evidence for `CanTransfer`, `buyGas` and debit across 2^256.

## 8. Results of the tests on 2026-11-14

Run from a worktree of `origin/main` `a99fcf7` with `CARGO_TARGET_DIR` outside the repository, `--locked`, debug profile, on a shared host.

| Command | Result |
| --- | --- |
| `cargo test --locked -p xone-primitives --test wide_balance` | 5 passed, 0 failed (`arithmetic_and_bounds`, `decode_rejects_non_canonical`, `matches_go_oracle`, `saturating_low_mutant_is_detected`, `round_trip_and_low`) |
| `cargo test --locked -p wide-balance-spike` | `leaf_slim_and_root_match_go` ok; `option_b_side_map` ok (40 rows) |
| `cargo test --locked -p wide-balance-spike --no-default-features` | the same two tests ok (identical to the default: `account-ext` is off) |
| `cargo test --locked -p wide-balance-spike --features account-ext` | `leaf_slim_and_root_match_go`, `option_a_account_ext` and `option_b_side_map` ok (option A with its one listed divergence) |
| `cargo test --locked -p xone-evm --test wide_matrix` | `matrix_equals_geth` ok (production adapter, 40 rows, no divergence list) |
| `cargo test --locked -p xone-state --test oracle` | 6 passed, 0 failed (includes `account_leaf_and_slim_equal_geth`, both `fifty_mutations_*`, `narrowing_a_wide_balance_changes_the_root`) |

Not run: `cargo test --release -p wide-balance-spike --test perf -- --ignored` (a timing test, ignored by default), the Go oracle (`oracle/run.sh`, needs a geth-XDC checkout and was not re-run, so the vectors are read as committed), and the archive tools (need the read-only archive host `xdc02`, not used in this lane). The `perf.rs` and archive numbers in section 5 and 7 are therefore the ones recorded in `REPORT.md`, not re-measured.

## 9. What is not proved

| Gap | Where it stands |
| --- | --- |
| Legacy `BALANCE` or `SELFDESTRUCT` touching a wide account in history | No such block found (all wide Apothem accounts are EOAs; no archive scan of 85 million blocks is possible with the allowed calls). The question "did legacy v2.6.8 push a value above 2^256 for `BALANCE`" is open; it needs another data source or an owner decision (`REPORT.md` "Legacy adjudication") |
| Mainnet | No mainnet archive. The claim rests on the supply argument |
| `0x89` and `0x90` special transactions and the TRC21 fee path with a wide account | Deferred in the matrix to issue #28. The routing and receipt code is on `main` (`crates/xone-evm/src/route.rs`, `special.rs`) but no wide-account row exercises it |
| Gas | Not compared; Prague, Osaka and Amsterdam rules are not run (Apothem at blocks 40M and 72M only) |
| Tracing | geth-XDC loses wide semantics when tracing wraps the StateDB (`REPORT.md` finding 1). xOne tracing must not change execution; no xOne test covers a traced wide transaction |
| Production overflow hooks | Stock revm ignores the result of `incr_balance` in `reimburse_caller` and `reward_beneficiary`. `WideJournal::fault` is the production answer; the case is unreachable under the supply bound and is not in the matrix |
| Wide-account lifecycle in storage | The side map needs a lazy load, a flush at commit and `forget` for deleted accounts (`REPORT.md` option table). `crates/xone-state` tests cover roots, not a restart in the middle of a block with a wide account |
| Vector provenance | The vectors are only as good as the geth-XDC oracle at the pin. The oracle and the reference client share a code line (ADR-002, shared failure domains) |

## 10. Reproduce

```sh
# vectors (writes the spike vectors and the primitives copy); needs a geth-XDC checkout at oracle/PIN
GETH_SRC=<checkout> spikes/wide-balance/oracle/run.sh
cargo test --locked -p xone-primitives --test wide_balance
cargo test --locked -p wide-balance-spike                           # option B on revm without account-ext
cargo test --locked -p wide-balance-spike --features account-ext    # options A and B
cargo test --locked -p xone-evm --test wide_matrix                  # production adapter
cargo test --locked -p xone-state --test oracle                     # production leaf and roots
```

Note: `REPORT.md` "Reproduce" says plain `cargo test -p wide-balance-spike` runs options A and B. It no longer does: `account-ext` is off by default (`spikes/wide-balance/Cargo.toml:9-15`), so option A needs `--features account-ext`, and `--no-default-features` is the same as the default.
