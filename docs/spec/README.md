# Pinned rule register and generated fork table (issue #6)

Two files here are generated. Do not edit them.

| File | Source | Content |
| --- | --- | --- |
| [fork-table.md](fork-table.md) | `crates/xone-chainspec/generated/forks.json` | every `params.ChainConfig` `*Block` row, the effective activations, the XDPoS parameters and the V2 rounds, for all five networks; `99,999,999,999` shown as `unscheduled`, `nil` as `-` |
| [rule-register.md](rule-register.md) | `spec/**/*.md`, `spec/chainspec/non-parameter-switches.md` | one row per rule page with its geth reference count and first reference; the non-parameter switches A1-D8 |

```sh
python3 docs/spec/gen.py           # regenerate
python3 docs/spec/gen.py --check   # exit 1 if a committed file is stale (for CI)
```

The script reads `conformance/PIN` and stops if it differs from the pin inside `forks.json`. It does not read geth. The chain of custody is: geth-XDC at the pin, then `conformance/exporter` (`go run .`), then `forks.json`, then this script. The exporter owns the numbers; this directory only renders them. The pages under `spec/` own the rules (see `spec/README.md`).

## What the register shows today

- 8 of 83 rule pages cite no geth `file:line`: `spec/bootstrap/{import,minimal-mode,snap-install,snapshot-format}.md` and `spec/ops/{ethstats,heartbeat,logs,metrics}.md`. They describe xOne-only behaviour. `spec/README.md` says a rule with no source line is not a rule, so each needs either a geth reference or an explicit "xOne only, no geth rule" statement. This PR does not edit those pages.
- Gaps G2 to G4 of `non-parameter-switches.md` (the `NetworkConstants` fields including `TIPIncreaseMasternodes`, the chain-id branches C3 and C4, and the `IgnoreSignerCheckBlocks` maps) still have no Rust representation, so the table cannot show them.
- The table is only as current as `forks.json`. Re-running the exporter needs read access to the private geth-XDC fork (`conformance/fetch-geth.sh`); it was not re-run for this PR.

## Not done here

The CI step that runs `gen.py --check` is not added (CI config is outside this docs-only lane). The owner decision is below.
