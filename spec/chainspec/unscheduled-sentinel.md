# `99_999_999_999` means declared but not scheduled

## Rule

geth-XDC declares some forks with the far-future placeholder `99_999_999_999` instead of leaving them `nil`. The fork is in the config and in `CheckConfigForkOrder`'s ladder, but no real chain reaches that height. A check that only asks "is the value present" would accept it as a real activation.

`nil` is a different state: geth `isBlockForked` returns false for a `nil` fork block.

| geth value | state in Rust | `is_active(n)` |
| --- | --- | --- |
| `big.NewInt(h)` with `h != 99_999_999_999` | `ForkBlock::At(h)` | `n >= h` |
| `99_999_999_999` | `ForkBlock::Unscheduled` | never |
| `nil` | `ForkBlock::Never` | never |
| key absent from the generated file | load error `MissingFork` | not constructible |

A value above the placeholder is a real height and stays `At`.

## Source

- `params/config.go:86-89` mainnet: `AmsterdamBlock`, `SigningTxBoundBlock`, `ValidatorOrderBlock`, `ParoleDedupBlock`.
- `params/config.go:1054-1057` apothem: the same four.
- `params/config.go:3897-3902` `isBlockForked`: `nil` is false, otherwise `s <= head`.
- `params/config.go:3000-3002`, `:3037-3039` plain rows use `isBlockForked`.

## Networks

mainnet (50) and apothem (51). Devnets leave these four `nil`.

## Rust

- `UNSCHEDULED_BLOCK`, `ForkBlock::from_table`, `ForkBlock::is_active`.
- Tests: `fork_block_semantics`, `placeholder_is_unscheduled_and_null_is_never`.

## Legacy

The comments on these rows say "scheduled with legacy" (#1721-#1723). The legacy XDPoSChain behaviour for the four forks is not checked here.
