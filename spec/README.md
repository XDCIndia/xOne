# xOne specification register

One page per rule that the Rust client must reproduce. The oracle is geth-XDC, not the legacy XDPoSChain: geth-XDC is the compatibility target, and where it differs from legacy XDPoSChain the page says so and the owner decides on chain history.

## The pin

Every `file:line` in this directory refers to `XDCIndia/go-ethereum` at

```
6fc8e8c2088b7f18264f5663036669d04fa7becf
```

The same SHA is in `conformance/PIN` (read by the exporter and `conformance/fetch-geth.sh`) and in `crates/xone-chainspec/genesis/PIN` (checked by a test against `generated/forks.json`). Moving the pin means: change `conformance/PIN`, run `conformance/fetch-geth.sh`, run the exporter, review the diff of `crates/xone-chainspec/generated/forks.json`, and re-read every page for moved lines.

## Page format

- Title: the rule, in one line.
- **Rule**: what geth-XDC does, in short sentences.
- **Source**: `file:line` at the pin, one bullet each.
- **Networks**: which of mainnet (50), apothem (51), devnet (5551), devnet551 (551) it affects.
- **Rust**: the table row, `ChainSpec` method or test that represents it.
- **Legacy**: only if legacy XDPoSChain differs, or the page cannot tell.

A rule with no source line is not a rule. A value that a page states must come from `generated/forks.json`; pages do not carry copies of table values except in a quoted source comment.

## Pages

| Page | Rule |
| --- | --- |
| [fork-table](chainspec/fork-table.md) | what the generated table holds and how it is produced and checked |
| [unscheduled-sentinel](chainspec/unscheduled-sentinel.md) | `99_999_999_999` means declared but not scheduled |
| [effective-istanbul-via-tipxdcx-cancellation-fee](chainspec/effective-istanbul-via-tipxdcx-cancellation-fee.md) | Istanbul and Petersburg fire at `TIPXDCXCancellationFee` |
| [eip1559-decoupled-from-london](chainspec/eip1559-decoupled-from-london.md) | the fee market, EIP-2929 and EIP-3860 follow `Eip1559Block` |
| [v2-config-by-round](chainspec/v2-config-by-round.md) | XDPoS v2 parameters are selected by round |
| [genesis-header](chainspec/genesis-header.md) | always-encoded XDC header fields and the genesis base fee |
| [non-parameter-switches](chainspec/non-parameter-switches.md) | every rule found at the pin that is not a plain `params` row |
| [account-leaf](state/account-leaf.md) | the XDC account RLP, its slim form and the secure-trie keys |
| [changesets](state/changesets.md) | whole-record changesets, the state of a retained block and side-branch execution |
