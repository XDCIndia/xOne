# V1 Prepare and Seal (XB-23)

## Rule

**Prepare** (geth `XDPoS.Prepare`). Coinbase and nonce zero (no vote), difficulty `len - hops`, `extraData` = 32 byte vanity (the miner's extra data, first 32 bytes, zero padded) + at a checkpoint (`number % epoch == 0 && number >= epoch`) the masternodes of the next epoch, ascending, minus the penalties of this epoch and of the four before + 65 zero bytes of seal; at a checkpoint `Penalties` holds this epoch's penalties and `Validators` the M2 tokens (4 ASCII bytes each, left padded with zeros); mix digest zero; time `max(parent + period, now)`.

**Seal.** The creator signs the seal hash (the header without the last 65 bytes of `extraData`, `validator`, `validators` and `penalties`) and writes it into the last 65 bytes (M1). **Past the first epoch** (`number > epoch`) the block also needs an M2 `validator`, which signs the same digest: when the creator is its own assigned M2 the same signature is the `validator`; when another masternode is, `validator` stays empty and that masternode co-signs (`cosign`; the block is complete then). A difficulty-1 block waits `random(0, (len/2 + 1) * 500 ms)` after its time before it is sent.

**Safety.** The seal intent is `Kind::SealV1` at the block number: the same height with another digest is refused. The block to be signed is written to `producer/pending.rlp` first; after a crash the same bytes are signed again (the signer is deterministic) and sent, so a restart neither double-signs nor loses the slot.

## Source

`consensus/XDPoS/xdpos.go:1386-1496` `Prepare` (`:1434-1437` vanity, `:1439-1480` checkpoint, `:1489-1492` time), `:1727-1858` `Seal` (`:1815` M1, `:1826-1832` M2 when self, `:1834-1840` delay, `:2780` `wiggleTime`), `:201-266` `SealHash`; `double_validation.go`; `miner/xdpos_worker.go:762-935` `commitV1Work` (`:787-797` the parent's second, `:900-935` the co-sign hand-off).

## Rust

`xone_builder::producer::seal_v1::{prepare, seal, cosign, Checkpoint, Sealed}`; `xone_consensus::v1::double_validation::assigned_validator`.

## Vectors and mutants

200 real Apothem V1 blocks rebuilt to the canonical hash, across the checkpoint 900, with M2 co-signed by another masternode (`producer_construction.rs`); a lone masternode is its own M2 (built with a throwaway key; no block of the replay sample has that case). Mutant: the creator is never its own M2.

## Not covered

The co-signature of a block that another masternode sealed is a function (`cosign`) but is not run by the node. `EpocBlockRandomize = 900` (`double_validation.go`) means a V1 chain with an epoch that is not a multiple of 900 cannot run past its first epoch; the local devnet therefore switches to V2 at its first epoch.
