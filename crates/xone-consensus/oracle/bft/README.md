# BFT oracle

A throwaway Go program that calls the pinned geth-XDC V2 BFT code (`PIN`, same commit as `conformance/PIN`) on explicit inputs and prints what it decides. It is not part of the build; the vectors `conformance/vectors/v2/bft-*.json` keep its output.

```
GETH_SRC=<checkout of XDCIndia/go-ethereum at PIN> PATH=/usr/local/go/bin:$PATH ./run.sh timer ../../../xone-chainspec/generated/forks.json ../../../../conformance/vectors/consensus/net2040.forks.json
GETH_SRC=... ./run.sh rule
GETH_SRC=... ./run.sh forensics
```

`GOTOOLCHAIN=local` (Go 1.25), no network. `run.sh` overlays two files for the build only (the checkout is not written): `export.go.txt` into `engine_v2` (a memory chain, `createEngine` over a memory database, and thin exports of `setNewRound`, `verifyVotingRule` and the epoch cache) and `countdown.go.txt` into `countdown` (`Fire`, which is `onTimeout`). Called: `createEngine`, `setNewRound`, `ExpCountDown.Reset/GetTimeout/onTimeout` (`timer`); `verifyVotingRule` and `isExtendingFromAncestor` (`rule`); `Forensics.ForensicsMonitoring` with `ProcessForensics`, `SetCommittedQCs`, `SendForensicProof`, `FindAncestorBlockHash` and `generateForensicsId`, the event read from the forensics feed (`forensics`). The oracle's epoch cache holds every epoch it seeds (the engine's holds 10).
