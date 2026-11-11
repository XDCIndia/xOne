# Snapshot format: the container and its rows

## Rule

A snapshot is one file that a node writes from its own state (`xone snapshot export`) and another
node checks and installs (`xone snapshot import`). The file hash proves the bytes. It does not prove
the chain: the anchor manifest inside it does (`spec/bootstrap-anchor.md`), and the pins file that
the importer is given is the trust root. Code: `xone_sync::bootstrap::{format, manifest, export}`.

## Container

```text
file  = "XSNP" | version u16 LE | chunk* | trailer
chunk = kind u8 | seq u32 LE | raw_len u32 LE | comp_len u32 LE | gzip(raw) | crc32c u32 LE
```

- `version` is `FORMAT_VERSION` (1), the number of the anchor (`bootstrap::FORMAT_VERSION`). A reader
  refuses another one as `UnsupportedFormat` before it reads a chunk.
- The CRC-32C (Castagnoli, polynomial 0x82F63B78) covers the 13 header bytes and the compressed
  payload. A chunk has at most 64 MiB raw and compressed, so a damaged length cannot make the reader
  allocate more.
- Gzip is `flate2` (level fast, zero timestamp): the same state gives the same bytes. The exporter
  closes a chunk at 1 MiB of raw rows (the tests use 8 KiB).
- `seq` counts from 0 and has no gaps.
- The trailer is the last chunk (kind 255). Its raw bytes are the RLP of
  `[manifest_hash, [[kind, seq, raw_len, crc]...], headers_first, records_first]`: every chunk before
  it, in order, the keccak256 of the manifest chunk, and the first block held of the headers and of
  the records (bodies and receipts, ADR-005 section 6.3 `first_block`). A byte after the trailer is
  an error.

A cut or damaged file is a typed error that says where to continue: `Truncated { chunks, offset }`
(the file ends inside a chunk; `offset` is the end of the last whole one), `BadCrc`, `BadSeq`,
`BadGzip`, `UnknownKind`, `TooLarge`.

## Chunks, in file order

| kind | rows (RLP list per chunk) | order |
| --- | --- | --- |
| 1 manifest | the RLP of the `Anchor`: chain id, genesis, checkpoint number and hash, state root, `created_at`, `max_age`, provenance (switch header, validators), the proof headers (QC chain on V2, seal chain on V1) | one chunk, first |
| 2 accounts | `[address, nonce, balance, code_hash]` | ascending `keccak256(address)` |
| 3 storage | `[address, slot, value]`, value not zero | ascending `(keccak256(address), keccak256(slot))` |
| 4 codes | `[hash, code]` | ascending hash |
| 5 side | `[signers, [[number, signer]..], [[gap block, candidates]..], [[block, [[sender, signed, ok]..]]..]]`: the V1 signer snapshot after the pivot, the V2 candidate pools of the gap blocks `pivot_plan` names, the sign transactions of the blocks in the window | one chunk |
| 6 headers | headers, `headers_first ..= pivot`, consecutive | ascending |
| 7 bodies | `[number, transactions, uncles]`, `records_first ..= pivot` | ascending |
| 8 receipts | `[number, receipts]`, `records_first ..= pivot` | ascending |

Rows carry the preimage (address, slot) and are sorted by the hashed key: the order a trie builder
wants, and the plain keys the store's flat tables take (snap's hashed tables have no preimages). The
balance is the true integer: a wide balance (above 2^256, up to 2^264 - 1) goes through in full.
The storage root is not carried: it is the root of the slots that follow, and the state root check
covers it.

## What the exporter does

1. Builds the anchor of block `at` from the node's own headers (`manifest::build_anchor`): V2, the
   nearest epoch switch at or below `at` is the provenance header and the proof runs to `at + 3`
   (the 3-chain that commits it; no epoch switch inside the proof). V1, the epoch checkpoint below
   `at` is the provenance and the proof runs until `ceil(2n/3)` distinct sealers have signed from
   `at` up. `at` defaults to the latest committed epoch switch (V2) or the highest block that has a
   proof (V1).
2. Asks `pivot_plan` what the first post-pivot blocks need (`spec/retention.md`): the window of
   headers, of bodies and receipts (one floor, the smaller of the two classes), the gap pools.
3. Reads the state of `at` through `Store::state_rows`, rebuilds its root and compares it with the
   pivot header's. A mismatch writes nothing (`StateRoot`).
4. Writes the file through `<out>.part` and renames it when complete.

It refuses, with a typed error and no output, a store that does not hold what the plan names
(`NotRetained`: a node that is itself pruned, or `at` older than its state window), a code the state
refers to but the node does not hold (`MissingCode`), a gap pool the plan names that the node does
not hold (`MissingGap`), and a block with no proof (`Proof`).

## Not in the file

The total difficulty, the receipts of blocks below the pivot beyond the window, and anything an
archive would give. The V1 signer snapshot, the gap pools and the sign transactions are not covered
by the manifest (see `import.md`, "Not authenticated").
