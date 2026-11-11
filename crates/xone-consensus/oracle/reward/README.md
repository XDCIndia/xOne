# Reward oracle

A throwaway Go program that calls the pinned geth-XDC reward functions (`PIN`, same commit as `conformance/PIN`) on explicit inputs and prints the reward map of each case. It is not part of the build; the vectors under `conformance/vectors/reward/` keep its output.

```
GETH_SRC=<checkout of XDCIndia/go-ethereum at PIN> PATH=/usr/local/go/bin:$PATH ./run.sh cases.json [net2040.forks.json]
GETH_SRC=... ./run.sh floats 63.42 50.27 ...     # floor(reward * 1e18) as big.Float computes it
```

`GOTOOLCHAIN=local` (Go 1.25), no network. Called: `CalculateRewardForSigner`, `...Fixed`, `...Proportional`, `GetRewardBalancesRate`, `GetRewardBalancesRateUpgrade` (`consensus/XDPoS/reward.go`) on a real `state.StateDB` that holds the 0x88 owners, voters and stakes of the case, and `util.RewardInflation`. The three lines of `HookReward` that turn a config reward into a chain reward (`eth/hooks/engine_v2_hooks.go:133-138`) are repeated verbatim, because they sit inside a closure. The window walk (`GetRewardForCheckpoint`, `GetSigningTxCount`, `GetSigningTxCountTiered`) needs a chain and an engine and is not called: the counts are inputs, and the archive's balances check the count.
