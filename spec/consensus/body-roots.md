# Block body checks and the roots recomputed from body and receipts

## Rule

Every commitment in the header is recomputed from the body or the receipts. No hash, root or bloom is taken from the input and compared with itself. The order is geth's.

1. Oversize: from `OsakaBlock`, the RLP length of the block is at most 8,388,608 bytes (`BlockOversized`). The length is the list of the header, the transaction list (a legacy transaction as its own list, a typed one as a byte string), the uncle list and, if not empty, the withdrawal list.
2. Signing transactions (from `SigningTxBoundBlock`, which is unscheduled on Apothem and mainnet): the block is at most 8,388,608 bytes and has at most `2 * xdpos.maxMasternodesV2` transactions sent to `0x...89` once `TIPSigning` is active (`BlockOversized`, `TooManySigningTxs`).
3. Uncles: there are none (`UnclesNotAllowed`), and the hash of the uncle list is the header's `sha3Uncles` (`UncleHashMismatch`).
4. Transactions: the ordered trie root of the consensus encodings (legacy: the RLP list; typed: type byte and payload) is `transactionsRoot` (`TxRootMismatch`). A transaction that does not decode to its type's field layout is `TxDecode`.
5. Withdrawals: if the header has `withdrawalsHash` the body has a withdrawal list (`WithdrawalsMissing`) with that root (`WithdrawalsRootMismatch`); if the header has none the body has no list (`WithdrawalsUnexpected`).
6. Blobs: if the header has `blobGasUsed`, the number of blob hashes in the transactions is `blobGasUsed / 131072` (`BlobGasMismatch`); if it has none there are no blobs (`BlobsUnexpected`).
7. Block access list: from `AmsterdamBlock` the header has `blockAccessListHash` (`AccessListHashMissing`); before it, it has none (`AccessListHashUnexpected`).
8. Receipts: one per transaction (`ReceiptCount`), each in geth's consensus encoding. The bloom of a receipt is derived from its logs; the OR of them is `logsBloom` (`BloomMismatch`); the ordered trie root of the receipt encodings, with those blooms, is `receiptsRoot` (`ReceiptRootMismatch`).

## Receipt encoding

A receipt is the list `[postStateOrStatus, cumulativeGasUsed, bloom, logs]`, and a typed receipt is its type byte (1 to 4) followed by that list. `postStateOrStatus` is a byte string: `0x01` for success, the empty string for failure, the 32-byte state root before Byzantium. A log is `[address, topics, data]`. XDC adds no receipt field: the 0x89 signing transaction gets an ordinary receipt (status, cumulative gas, a log with only the contract address), and the fields `from`, `contractAddress`, `effectiveGasPrice` and the like are not part of the encoding. The decoder refuses what the encoder would not write (trailing bytes, an unsupported type).

The trie key of item `i` is `rlp(i)`; the root is the ordered trie over the encoded items (`ordered_trie_root_encoded` of `alloy-trie`), as geth's `DeriveSha`.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `core/block_validator.go:52` `ValidateBody`: `:54` Osaka size, `:61` signing bound (`:292-310` `checkSigningTxBound`), `:72` uncles, `:75` uncle root, `:78` transaction root, `:83-96` withdrawals, `:100-118` blobs, `:124` access list.
- `core/block_validator.go:198` `ValidateState`: `:216` bloom, `:226` receipt root. `gasUsed` and `stateRoot` need execution and are not in this slice.
- `core/types/block.go:398` `EncodeRLP` and `:509` `Size`; `:532` `CalcUncleHash`.
- `core/types/hashing.go:108` `DeriveSha`; `core/types/transaction.go:673` `EncodeIndex`; `core/types/withdrawal.go:61`.
- `core/types/receipt.go:91` `receiptRLP`, `:123` `EncodeRLP`, `:144` `MarshalBinary`, `:240` `statusEncoding`, `:362` `Receipts.EncodeIndex`.
- `core/state_processor.go:856` `RouteTransaction` (a transaction to `0x89` after `TIPSigning` is a signing transaction); `common/xdc_contracts.go:19` `BlockSignersBinary`.
- `params/protocol_params.go:227` `MaxBlockSize`.

## Networks

All four.

## Rust

- `xone_consensus::verify::{Body, BodyVerifier, Receipt}` (`crates/xone-consensus/src/verify/body.rs`). `Receipt::decode` and `encode_consensus` are the receipt codec; `BodyVerifier::verify` runs the list above and never reads a header root as a value to return.
- Corpus: `crates/xone-consensus/tests/corpus.rs`; rules starting `body_` in `conformance/negative-gen/src/main.rs`.

## Not covered

- `gasUsed`, `stateRoot`, `requestsHash` and the block access list contents are results of execution (XB-06 and later). `ValidateState` checks them after the receipts.
- A blob transaction is read for its blob hash count only. No vector has one.
- The size of a block with a non-empty withdrawal list follows `extblock`'s optional tail as written here; no vector has a non-empty list.
