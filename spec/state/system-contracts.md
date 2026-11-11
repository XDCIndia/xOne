# The validator, block-signer and randomize contracts are read from storage slots

**Rule.** Consensus and rewards read three system contracts straight from state, without the EVM. The slot arithmetic is `core/state/state_reader.go`: a mapping entry is `keccak256(pad32(key) ++ pad32(slot))` (`:18-24`); element `i` of a dynamic array whose length is at `slot` is `keccak256(pad32(slot)) + i * size`, a wrapping add (`:26-31`). A word read as an address is its low 20 bytes. The slot numbers are the Solidity layout of the compiled contracts ("KYC Layer added", `core/state/statedb_utils.go:72-73`).

| contract | address | state | slot | reader (geth) |
| --- | --- | --- | --- | --- |
| validator | 0x88 (`common/xdc_contracts.go:23`) | `candidates` address array | 8 | `GetCandidates` (`statedb_utils.go:95-112`), zero entries skipped |
| | | `validatorsState[c].owner` | `keccak(pad32(c) ++ pad32(1)) + 0` | `GetCandidateOwner` (`:114-121`) |
| | | `validatorsState[c].cap` | same `+ 1` | `GetCandidateCap` (`:123-130`) |
| | | `voters[c]` address array | mapping at 2 | `GetVoters` (`:132-149`) |
| | | `validatorsState[c].voters[v]` | `keccak(pad32(v) ++ pad32(base + 2))`, base the `validatorsState[c]` location | `GetVoterCap` (`:151-157`) |
| block signers | 0x89 (`:19`) | `blockSigners[hash]` address array | mapping at 0 | `GetSigners` (`:22-40`) |
| randomize | 0x90 (`:28`) | `randomSecret[m]` `bytes32[]` | mapping at 0 | `GetSecret` (`:48-63`) |
| | | `randomOpening[m]` `bytes32` | mapping at 1 | `GetOpening` (`:65-70`) |

0x89 is wiped at `TIPSigning` (`special-transactions.md`), so its readers are empty from that block. The owner of a candidate is `None` when the word is the zero address, which fee routing treats as no owner (`core/state_transition.go:909`). Decrypting a secret with its opening (`contracts/utils.go`) is not part of the reader.

**Rust.** `xone_state::system_contracts::{validator_0x88, signer_0x89, randomize_0x90}` over `StateRead`, with `mapping_slot` and `dyn_array_slot`.

**Vectors.** `conformance/vectors/special/system_contracts.jsonl`: per sampled block the archive's `eth_call` answer and the storage that call read (`debug_traceCall` `prestateTracer`). The reader runs on that storage and must give the answer; a reader that is off by one slot reads zero and fails (`crates/xone-state/tests/system_contracts.rs`). Candidates, owner, cap, voters and voter cap are compared for 100 blocks of the whole history, signers for 100 blocks before `TIPSigning`, secrets and opening for the V1 epochs after `TIPRandomize` where a masternode has one. `XDPoS_getCandidates` is not used: it returns the snapshot signers (`consensus/XDPoS/api.go:184-199`), not the contract's list.
