# V2 forensics: two conflicting QCs

## Rule

After a block is committed by the 3-chain rule, geth compares the QC that committed it with the QCs of the previous commit (`ForensicsMonitoring`). The machine does the same, once for each commit, inside `processQC` (`Forensics::monitor`).

1. `SetCommittedQCs`: the node remembers three QCs, oldest first: the QC in the parent of the proposed block, the QC in the proposed block, and the incoming QC. The two headers must be on one chain in order and the incoming QC must certify the last of them, or nothing is stored.
2. `ProcessForensics` (with the QCs of the previous commit, if there were three): the incoming QC and its two ancestors (`findAncestorQCs`: the QC in the block of the incoming QC, then the QC in the block of that one) are compared with them. If the first QCs of the two sets are on one chain (`checkQCsOnTheSameChain`: walk the set with the higher first block down to the number of the other and compare block infos), nothing is reported.
3. Otherwise two QCs prove the conflict: the first pair of equal round, the committed set first (`findQCsInSameRound`), or, with no equal round, the QC of the higher-round set that is the last one not below the third QC of the lower-round set, and that third QC (`findAncestorQcThroughRound`, at most 1,800 steps).
4. `SendForensicProof`: the two QCs are put in order of round; the common ancestor of their blocks is found by walking the higher block down and then both blocks together (`FindAncestorBlockHash`); the proof says whether the two blocks are in different epochs (the epoch switch blocks differ), the diverging block's hash and number, and for each QC the hash path from the diverging block and the signers that recover.
5. The id is `generateForensicsId`: `diverging hash : smaller QC's block hash : larger QC's block hash`, each `Hash.Hex()`, `0x` and lower-case hex, joined with `:`.

One event for each id: a conflict seen again (the same two QCs) is not reported again. geth emits an event each time; the machine keeps the last 256 ids. An error anywhere in the walk (a header missing) ends the check without an event, as in geth, where the monitor runs in its own goroutine and only logs.

The vote-equivocation proof of geth (`DetectEquivocationInVotePool`, `ProcessVoteEquivocation`) is a different proof type with its own id and is not made.

## Source

- `engine_v2/forensics.go:50-53` `ForensicsMonitoring`; `:56-90` `SetCommittedQCs`; `:98-139` `ProcessForensics`; `:142-211` `SendForensicProof`; `:213-245` `findAncestorQCs`; `:248-282` `checkQCsOnTheSameChain`; `:285-294` `findQCsInSameRound`; `:296-316` `getQcSignerAddresses`; `:321-362` `findAncestorQcThroughRound`; `:364-418` `FindAncestorBlockHash`; `:420-423` `generateForensicsId`.
- `engine_v2/utils.go:586-597` the call after a commit (`go ForensicsMonitoring`).

## Rust

`xone_consensus::v2::forensics::{Forensics, Proof, Side, forensics_id}`; `Effect::Forensics(Proof)`.

## Vectors

`conformance/vectors/v2/bft-forensics.json` (oracle command `forensics`, the pinned `ForensicsMonitoring` on 150 generated chains with a fork, epoch switch info seeded for every block, the event read from the geth feed): 109 events and 41 quiet cases (same chain, or the QC sets are not in conflict). For each: the id, the diverging hash and number, `across_epoch`, both hash paths and both QC block hashes equal the oracle's. `tests/bft_vectors.rs::two_conflicting_qcs_make_one_event_with_the_id_of_geth` also asserts that the same conflict a second time makes no event. In the simulator any `Forensics` effect is a failure: with at most f Byzantine validators two committed blocks never conflict.

## Not covered

- QC signers: the QCs of the vectors have no signatures; the signer list is made with the signature scheme of the machine (`recover`), and a signature that does not recover is skipped (geth would index a nil key).
- The vote-equivocation proof (above).
