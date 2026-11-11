# V2 follower vectors (XB-11) and BFT vectors (XB-15)

Read by `crates/xone-consensus/tests/v2_vectors.rs`, `v2_header.rs` and (the `bft-*` files) `bft_vectors.rs`. Never edit by hand: the archive builders are in `build/`, the pinned-geth oracle is `crates/xone-consensus/oracle/v2` (PIN = `conformance/PIN`).

| file | from | what |
| --- | --- | --- |
| `gap-apothem.json`, `gap-net2040.json` | archive (`xdc02:21105`, `:10071`, read-only, at most 4 requests per second, 2026-11-11) | 100 and 20 V2 epoch switches: 0x88 words at the gap block (inputs), switch header `Validators` and `Penalties` (expected), plus one `bootstrap` record each (the first V2 block and the V1 switch block's `extraData`) |
| `leader-apothem.json`, `leader-net2040.json` | archive | 50 consecutive blocks across a switch: round, coinbase, the two masternode lists |
| `sort.json` | oracle `sort` | `state.GetCandidates` + `sortCandidatesByStake` on a real StateDB, 30 cases up to 464 candidates |
| `calc.json` | oracle `calc` | `calcMasternodes` and `GetStandbynodes` on a seeded gap snapshot, 19 cases |
| `epoch.json` | oracle `epoch` | `IsEpochSwitch`, `isEpochSwitchAtRound`, 638 cases |
| `cert.json` | oracle `cert` | `verifyQC`, `verifyTC`, `VerifySyncInfoMessage`, `rlp.DecodeBytes`: 220 cases, 22 decodings, 24 threshold edges |
| `commit.json` | oracle `commit` | `commitBlocks` on 40 chains, 1,600 cases |
| `midadd.json` | oracle `midadd` | `yourturnAligned` across a mid-epoch masternode addition, 17 cases |
| `bft-timer.json` | oracle `bft timer` | the round timer of the pinned engine (`setNewRound`, `Reset`, `GetTimeout`, `onTimeout`): every V2 config row of mainnet, Apothem, devnet, devnet551 and net2040, and synthetic rows; 300 steps |
| `bft-rule.json` | oracle `bft rule` | `verifyVotingRule` on 3,027 explicit inputs, and the net2040 619,712/619,713 sibling shape (9 cases) |
| `bft-forensics.json` | oracle `bft forensics` | `ForensicsMonitoring` on 150 generated chains with a fork: the event, its id and its paths (109 events) |

## Regenerating

```
ssh -N -L 127.0.0.1:17105:127.0.0.1:21105 -L 127.0.0.1:17071:127.0.0.1:10071 xdc02 &   # not needed by build/, which calls ssh itself
cd conformance/vectors/v2/build
python3 -I gap.py apothem ../gap-apothem.json
python3 -I gap.py net2040 ../gap-net2040.json
python3 -I leader.py apothem ../leader-apothem.json
python3 -I leader.py net2040 ../leader-net2040.json
cd ../../../../crates/xone-consensus/oracle/v2
GETH_SRC=<geth-XDC checkout at PIN> ./run.sh sort > ../../../../conformance/vectors/v2/sort.json   # likewise calc epoch cert commit midadd
cd ../bft   # the BFT vectors; the oracle is crates/xone-consensus/oracle/bft (same PIN)
GETH_SRC=<geth-XDC checkout at PIN> ./run.sh rule > ../../../../conformance/vectors/v2/bft-rule.json   # likewise forensics
GETH_SRC=<geth-XDC checkout at PIN> ./run.sh timer ../../../xone-chainspec/generated/forks.json ../../../../conformance/vectors/consensus/net2040.forks.json > ../../../../conformance/vectors/v2/bft-timer.json
```

`build/*.py` import `../../reward/build/arch.py` (ssh + curl, cached in `cache.db`, which is ignored). Python 3 with `pycryptodome`. The oracle needs Go 1.25 with `GOTOOLCHAIN=local`; its overlay adds `export.go.txt` to the engine package for the build only.

## Gap record

`number` of the switch block, `round` of its header, `gap` the gap block number, `gap_hash`, `words` (`[slot, value]`, every nonzero word read at the gap block: the array length at slot 8, each array element, each candidate's `validatorsState[c].cap`), `validators` and `penalties` (header fields, equal to `XDPoS_getMasternodesByNumber`, checked by the builder).

For a switch whose epoch starts at the V2 switch block, the gap block is a V1 block and the pool is the V1 `extraData` list (`bootstrap.switch_extra`); the `words` of those records are kept but not used.
