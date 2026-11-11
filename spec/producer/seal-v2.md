# V2 Prepare and Seal (XB-23)

## Rule

**Prepare** (engine_v2 `Prepare`). The parent must be the block of the highest QC (`ErrNotReadyToPropose` otherwise). `extraData` = version byte 2 + RLP `{round, highestQC}` (the QC of the first V2 block is the round-0 QC of the switch block with no signatures). Coinbase = the signer, difficulty 1, nonce and mix zero. At an epoch switch `Validators` and `Penalties` are the new list and the penalties (`calcMasternodes`). Time = `parent + minePeriod(round)` (the config row of the round), or now once that is past; the worker does not build before `parent + minePeriod` has passed.

**Seal.** The signature over the V2 signature hash (the header with the whole `extraData`, `validators`, `penalties` and the base fee; not `validator`) goes in `validator`; the round is marked mined. Here the mark is the unit's `Propose` slot, **decided and written before the signature exists**; the signature leaves the unit only after it.

**After the seal:** the block is imported (full header check and execution), sent as `NewBlock`, and fed to the step machine as `Proposal`, which votes through the same unit; the vote returns as `Event::Vote`, the QC forms, the round advances, and the next leader builds on that QC. The machine's `PersistIntent`/`Sign`/`Send` are run by `bft::drive::drive`, which hands each intent to `Sealer::sign_joined`: the machine's record (its QC, TC and lock observations) is **joined** into the unit's record before the write, so neither copy lowers the other.

## Source

`engine_v2/engine.go:651-766` `Prepare` (`:686` not ready, `:755-758` time, `:761` coinbase), `:769-806` `Finalize`, `:812-845` `Seal` (`:835` validator, `:843` `markSelfMined`), `:285-316` `sigHash`; `engine_v2/mining.go:24-71`; `miner/xdpos_worker.go:377-935` `commitNewWork` (`:584` the mine period, `:895-935` `commitMinedBlock`).

## Rust

`xone_builder::producer::seal_v2::{prepare, seal}`; `xone_consensus::{unit::SafetyUnit, v2::bft::drive::drive, v2::safety::SafetyRecord::join}`; node: `xone_node::services::producer`.

## Vectors and mutants

200 real Apothem V2 blocks (with an epoch switch) rebuilt to the canonical hash. Mutant: a wrong round in `extraData`. Safety mutants (`tests/producer_mutants.rs`, `tests/producer_unit.rs`): a round proposed twice (guard off, record ignored), signing before persisting.

## Not covered

The timeout path across epoch boundaries (`epoch_of_round` answers with the last known epoch), the reward window of the second V2 epoch on a chain with fewer than two V1 checkpoints (`reward::v2::locate`), the producer tick budget with a cold state cache, `Stop()` inside a long execution (a stop is honoured between transactions and phases).
