# V2 3-chain commit

## Rule

When a QC certifies block `B` (processQC), the commit rule looks at `B`, its parent `P` and its grandparent `G`:

1. If `number(B) - 2 <= V2.SwitchBlock`, nothing is committed.
2. `P` is read by `B.parentHash`; absent is an error. If `round(B) - 1 != round(P)` (unsigned, wrapping), nothing is committed.
3. `G` is read by `P.parentHash`; absent is an error. If `round(B) - 2 != round(G)`, nothing is committed.
4. If something is committed already and its round is `>= round(G)` or its number is `>= number(G)`, nothing is committed.
5. Otherwise `G` is committed, and becomes the highest committed block.

Three blocks of three consecutive rounds are needed: a timeout between them breaks the chain, and the rule waits for the next three. The rounds are those of the headers, read by hash, so a sibling at the same height is never mixed in. The blocks of a V1 header or the switch block have no V2 round; rule 1 keeps them out of the walk.

## Source

- `engine_v2/utils.go:476-528` `processQC` (lock QC, commit, round advance); `:530-598` `commitBlocks`: `:532` the switch guard, `:554` parent round, `:575` grandparent round, `:583` already committed.
- The trusted-checkpoint window (`inCheckpointCatchup`, `:268-289`) is a bulk-sync device of this fork and not a rule here.

## Rust

`xone_consensus::v2::commit::{commit_blocks, Link, Headers}`. The state is the highest committed `Link`, passed in and returned by the caller; no global.

## Vectors

`conformance/vectors/v2/commit.json` (oracle command `commit`): the pinned geth `commitBlocks` run on 40 seeded chains whose rounds sometimes skip, for every V2 block with five already-committed states (none, or the block 0..3 places below), and 40 proposed blocks whose parent is not held: 1,600 cases, 268 of them commit. Mutants: the grandparent round check, and the already-committed test.

## Not covered

- Lock QC, highest QC, round advance and forensics (`processQC` beyond the commit) are the voting engine (XB-15).
