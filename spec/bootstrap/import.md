# Snapshot import: what is checked, in what order

Code: `xone_sync::bootstrap::import::{verify, import}`, the node's files in
`xone_node::snapshot::import_datadir`. Format: `snapshot-format.md`.

## Order

1. **Pins.** An import without a pins file is `NoPins`, before the file is opened. A pins file lists
   `<block number> <0x block hash>` per line (`#` starts a comment): the hashes of trusted epoch
   switches or checkpoints. `verify_anchor` takes a match with the checkpoint or the provenance
   switch. This is the one fact the manifest cannot give about itself
   (`spec/bootstrap-anchor.md`, "What is still trusted"); a validator-set pin is not supported
   (a set is checked through the hash of its switch header). `--max-age-cap` is the verifier's cap
   on the age of a manifest (default 7 days).
2. **Version and manifest.** The file version, then the manifest chunk, then `verify_anchor`:
   format, chain, genesis, freshness, pin, binding, certificates (V2) or seals (V1). The first
   failure is the error. Nothing has been written: not the staging file, not the datadir. Tests:
   wrong chain, wrong genesis, wrong root, stale, from the future, future manifest version, future
   file version, forged certificate, under-threshold certificate, not pinned, no pins.
3. **Chunks.** Each chunk is checked by CRC-32C and appended to the staging file
   (`<datadir>/xone/snapshot.part`), which is by construction a prefix of the snapshot made of whole
   chunks. The trailer must name exactly the chunks read and the manifest hash.
4. **Content.**
   - The trailer's first blocks are not above what `pivot_plan` asks for (`ShortWindow`; a datadir
     that holds more is accepted, ADR-005 section 12: retention only grows without migration).
   - Headers consecutive, linked by parent hash, ending at the manifest's checkpoint hash, equal to
     the proof headers where they overlap.
   - Every body is checked against its header (`BodyVerifier::verify_body`: transaction root, uncle
     hash, sizes), every receipt list too (`verify_receipts`: count, bloom, receipt root).
   - Account, slot and code rows in ascending key order, no zero slot, no slot of an absent account,
     every balance inside the legal bound, every code hashes to its name and is the code of some
     account and every account's code is there.
   - The rebuilt state root equals the manifest's (`StateRoot`).
   - The gap pools are exactly the ones the plan names.
5. **Write.** `Store::install_snapshot`: one generation holds the state, the canonical index, the
   headers, the records, the anchor manifest (the format version is in it) and the segment manifest
   whose `first_block` is the first retained block. The node's files: the state checkpoint first
   (`state/ckpt-<pivot>.json.gz`, which also holds the codes and the side data), then the segments,
   `anchor.rlp`, and `head.json` last: the datadir exists when `head.json` does. An import into a
   datadir that holds a chain is `NotEmpty`. The staging file is deleted after the write.

## Resume

The file ends inside a chunk: `Truncated { chunks, offset }`. It ends between chunks with no
trailer: `Incomplete { chunks, offset }`. In both cases the staging file holds the whole chunks
and the next run reads the input from the end of the staged prefix (`stats.resumed_from`), after
checking that the first chunk is the same manifest; staging of another snapshot is dropped. A kill of
the process before the write leaves no datadir and a valid prefix. A flipped byte anywhere is refused
and nothing after it is trusted. Tests: cuts after five different chunks and inside one.

## Not authenticated

The manifest authenticates the state (root), the window headers (chain to the checkpoint), the
bodies and receipts (roots in those headers) and the codes (hash and use). It does not authenticate:

- the **V1 signer snapshot** (`signers`, `recents`) and the **sign transactions**. A forged one
  makes the node refuse the true reward block or seal check, not accept a false block.
- the **gap pools**: they are the 0x88 state at the gap blocks, which the importer does not have.
  Same open point as `spec/bootstrap-anchor.md` "Not covered": a forged pool changes the list that the
  first epoch switch derives, which differs from the signed header's list, so that block is refused.

Residual trust is the pins file and these three items; the first is the operator's choice, the
others are detected at the first block that reads them.
