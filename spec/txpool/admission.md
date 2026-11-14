# Admission: what a transaction must satisfy to enter the pool, in geth's order

All lines are `XDCIndia/go-ethereum` at `6fc8e8c2` (`core/txpool/validation.go` is `validation.go`, `core/txpool/legacypool/legacypool.go` is `legacypool.go`). The Rust is `xone_builder::txpool::validate` and `Pool::add_batch` (`pending.rs`). The pool takes remote transactions only: geth has no locals here, and xOne has no journal.

**Rule.** A transaction is offered as its consensus encoding. It is decoded and its sender recovered (`PoolTx::decode`, which uses `xone_executor::tx::decode`: legacy with a homestead or EIP-155 signature, type 1, type 2; any other type is `transaction type not supported`; trailing bytes after a typed payload are refused). The hash is the keccak of the encoding and the size its length. Then, in this order (each is the first error returned; a transaction that breaks two rules gets the first):

1. Already in the pool: `already known` (`legacypool.go:1244-1265`).
2. `ValidateTxBasics` (`legacypool.go:585-603`, `validation.go:76-214`), against the head header only:
   1. size above 128 KiB (4 slots of 32 KiB, `legacypool.go:51-57`): `oversized data` (`validation.go:83`);
   2. type gates (`:88-99`): a typed transaction before Berlin, before `Eip1559Block` (the XDC rule, the row alone), and a type 2 before London are refused. At Apothem `Berlin` and `London` are 61,290,000 and `Eip1559Block` 71,550,000, so the third gate decides;
   3. contract creation with more than 49,152 bytes of initcode from `Eip1559Block`: `max initcode size exceeded` (`:113`, `vm.CheckMaxInitCodeSize`, `core/vm/common.go:29-40`: EIP-3860 on XDC is the `Eip1559Block` row);
   4. from Osaka, gas above 2^24: `transaction gas limit too high` (`:117`). No network has an Osaka block;
   5. gas above the head's gas limit: `exceeds block gas limit` (`:129`);
   6. tip above fee cap: `max priority fee per gas higher than max fee per gas` (`:141`);
   7. nonce 2^64-1: `nonce has max value` (`:150`);
   8. a special transaction from an address that is not authorised: `special transaction sender is not a signer` (`:154`, see `special-txs.md`);
   9. gas below the intrinsic gas of the message at the head's rules (`:174-180`). 68 gas per non-zero calldata byte until Prague, 16 from it (`XDCPreEIP2028Calldata`, `core/state_transition.go:71-80`, fed to `IntrinsicGas` only); from Prague also the EIP-7623 floor, `insufficient gas for floor data gas cost` (`:182-196`). Rust: `xone_evm::gas::IntrinsicTables`, built once per head, over `gas_params` and `floor_params`;
   10. a regular transaction whose tip is below the pool minimum: `transaction gas price below minimum` (`:211`). The minimum is `Config.PriceLimit` (1 wei) or what `SetGasTip` set. A special transaction is exempt.
3. `validateTx` against the head state (`legacypool.go:640-698`, `validation.go:339-422`):
   1. nonce below the account's: `nonce too low` (`:347`);
   2. balance below the cost `gas * gasPrice + value`: `insufficient funds for gas * price + value` (`:380`). The balance is `StateDB.GetBalance`, the **low 256 bits** of a balance above 2^256-1 (`xone_primitives::WideBalance::low`; the EVM and the pool see the wrapped value, `spec/evm/wide-balance.md`). A cost above 2^256-1 can never be paid and gets the same error;
   3. the cost plus what the pool already holds executable for the sender (`list.totalcost`), with the cost of the transaction at the same nonce taken out when this is a replacement (`:385-398`): same error;
   4. regular transactions only: the gas price (the fee cap for type 2) below the tier floor of the **next** block: `transaction gas price under xdc tier minimum` (`:406-421`). The floor is 12.5 gwei from `Gas50x`, 625 gwei from `Gas2500x`, and 0.25 gwei (`common.DefaultMinGasPrice`, `common/constants.go:15`) before `Gas50x`. It reads the **effective** `Gas50x` (`xone_chainspec` `effective.gas50x`: 56,828,700 on Apothem, not the empty `gas50xBlock` row) and the same tier function as the header base fee, but without the `Eip1559Block` gate (`params/xdc_gas.go:36-42`);
   2a. TRC21 token-fee transactions (`validation.go:360-379`): when `to` is a token registered in the TRC21 issuer (`trc21_issuer::capacities`, read once per head as `Pool.trc21`), `trc21_issuer::validate_trc21_tx` must pass (`insufficient funds`), the cost is `TRC21GasPrice(head + 1) * gas + value` and the balance of steps 2 and 3 is `balance + capacity`. On reset, `List::filter` keeps such a transaction while `balance + capacity` covers that cost (`list.go:397-408`), in promotion and demotion alike;
   5. a sender that has code (an EIP-7702 delegation) may have one executable transaction and none out of order (`checkDelegationLimit`, `legacypool.go:700-740`): `gapped-nonce tx from delegated accounts`, `in-flight transaction limit reached for delegated accounts`. A never-touched account counts as having no code.
4. The fast path of special transactions (`special-txs.md`), then the fill checks and the insertion (`ordering-and-eviction.md`).

**Rust.** `Pool::add_batch(state, now, &[Arc<PoolTx>])` does geth's `Add(txs, sync)` (`legacypool.go:1244-1340`): the checks of step 2 on every transaction, then step 3 and the insertion of each under one call, then one reorg run for the accounts that were not replaced (`promote_executables`, `truncate_pending`, `truncate_queue`, the events). `Pool::add_raw` decodes first. `state` is the state of the head given to `Pool::new` or the last `Pool::reset`; `now` is the time in seconds. The pool reads no clock.

**Not ported.**

- The blacklist (`legacypool.go:641-655`, `common.IsInBlacklist`): the denylist addresses are not in xOne yet, and block processing has no denylist rule to match (`denylistBlock` is a parsed row only).
- Set-code (type 4) and blob (type 3) transactions: `xone_executor::tx` does not decode them, so they are `transaction type not supported`, as the executor would refuse them in a block.
- A head at which xone-evm has no rules (Amsterdam: net2040 is past it): `Pool::new` and `Pool::reset` return `Error::Spec`. The pool cannot be opened there (`crates/xone-builder/tests/archive_candidates.rs`).
- Signature errors come before the size check here (the signature is recovered at decode); geth recovers it after the cheaper checks. Only the error text of a transaction that is both oversized and badly signed differs.

**Vectors.** `conformance/vectors/txpool/scenarios.json`: 53 scripted and 60 seeded scenarios (2,460 steps) built from the pinned legacy pool by `crates/xone-builder/oracle/` (`run.sh`; see `ordering-and-eviction.md`). The scripted ones that decide admission: `adm-*`, `gas-*`, `size-oversize`, `price-min-tip`, `floor-tier-2500x`, `floor-tier-50x`, `floor-before-50x`, `floor-dynamic-fee-uses-fee-cap`, `type-*`, `tx-homestead-signature`, `delegated-sender`, `batch-mixed`. Test `crates/xone-builder/tests/oracle_scenarios.rs`; `tests/rules.rs` names the wide-balance and floor rules one by one.
