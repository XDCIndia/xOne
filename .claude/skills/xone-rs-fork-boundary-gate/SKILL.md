---
name: xone-rs-fork-boundary-gate
description: The fork-boundary and epoch-switch gate of the xOne Rust rebuild. Lists the Apothem and mainnet fork blocks and the epoch switches that must pass at 0 mismatches before a milestone closes, and says how to refresh the list from geth-XDC params/config.go. Use when a milestone is about to close, when a fork value is added or changed, when geth-XDC is re-pinned, or when the task says fork boundary, fork gate, chainspec check, epoch-switch gate, "can we close M2/M3/M4".
---

# xone-rs-fork-boundary-gate

XDC forks are block-gated. A client that reaches a boundary with a missing or time-gated value computes a wrong root there or soon after. This gate runs before a milestone is closed, and again after every re-pin of geth-XDC.

## Operating rules

1. The values below are a snapshot at geth-XDC `main` `5d6ba3664`. **Always re-read them from the pinned geth before the gate**; if they differ, the pinned geth wins and this file gets a PR.
2. Sources at the pin: `params/config.go` (network configs; Apothem block at lines 1040-1057, mainnet near line 85), `consensus/XDPoS/network_constants.go:48-98`, and the XDC rule gating in `params/config.go:4081-4110`.
3. A fork value of `9_999_999_999` or `99_999_999_999` means "not scheduled". It is still present in the table; a missing field is a start-up error.
4. There are no time-based forks on XDC. A timestamp in the chainspec is a defect.
5. The gate is 0 mismatches. There is no "known difference".

## Boundaries (snapshot)

| Rule | Apothem (51) | Mainnet (50) |
|---|---|---|
| TIP2019 | 1 | 1 |
| TIPSigning | 3,000,000 | 3,000,000 |
| TIPRandomize | 3,464,000 | 3,464,000 |
| TIPIncreaseMasternodes | 5,000,000 | 5,000,000 |
| No-halving reward, XDCX, lending, TRC21 fee | 23,779,191 | 38,383,838 |
| V2 switch, Gas50x | 56,828,700 | 80,370,000 |
| Berlin, London, Merge, Shanghai (PUSH0) | 61,290,000 | 76,321,000 |
| XDCX miner disable | 61,290,000 | 80,370,000 |
| XDCX receiver disable | 66,825,000 | 80,370,900 |
| EIP-1559 (also EIP-2929, 3529, 3860) | 71,550,000 | 98,800,200 |
| Cancun | 71,551,800 | 98,802,000 |
| Reward upgrade, penalty upgrade, Gas2500x, dynamic gas limit | 83,600,000 | not scheduled |
| Prague (EIP-2028 calldata price starts) | 84,333,000 | not scheduled |
| Proportional reward | 84,733,000 | not scheduled |
| Osaka, Amsterdam | not scheduled | not scheduled |

## Other mandatory blocks

- V1: block 1 and the first checkpoint blocks 900 and 1800 (Apothem 1800 changes the checkpoint masternode list).
- V2 bootstrap: the first three epoch switches after the V2 switch on each network. Mainnet: 80,370,001, 80,370,839, 80,371,724. Apothem: 56,828,701, 56,829,393, 56,830,293. The first two use the bootstrap order, the third the steady order.
- Reward checkpoints 85,169,004 and 85,169,902 (Apothem, proportional reward).
- Regression blocks from the campaign: 83,502,001; 85,188,255; 85,193,190; 85,197,568; 85,209,327.
- net2040: the fork walk and the window 619,491-619,713.

## Steps

1. **Refresh.** Run the exporter's `forks` command against the pinned geth. Diff its output with `crates/xone-chainspec` generated tables and with the table above.
2. **Window.** For each boundary `B` of the milestone's range, run `xone-rs-diff-exec` on `B-10 .. B+10`, and on the next epoch switch after `B`.
3. **Epoch switches.** Run the consensus pass on every epoch switch in the milestone range: `Validators`, `Penalties`, reward per owner.
4. **Start-up check.** Remove one fork value from a copy of the chainspec; the client must refuse to start.
5. **Record.** Post the table of boundaries with result, commit and pin on the milestone issue.

## Which milestone needs which rows

- M1: genesis hashes; the start-up check.
- M2: every row up to the V2 switch minus one, and the V1 blocks.
- M3: the V2 switch and the V2 bootstrap switches.
- M4: every later row on Apothem, and the regression blocks.
- M8: every mainnet row.

## Done

Every row in scope shows 0 mismatches at the current commit and pin, the refresh showed no drift (or the drift is merged), and the result is on the milestone issue.
